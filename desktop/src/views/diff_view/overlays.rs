//! What floats over the rows, and the bars around them: the status bar with
//! the search prompt, the comment editor, the palette and the `?` help.

use gpui::{div, prelude::*, px, AnyElement, Context, Div, Hsla, MouseButton, SharedString, Window};
use shelbi_app::diff::draft::{CommentDraft, DraftTarget};
use shelbi_app::diff::help::help_sections;
use shelbi_app::diff::palette::{DiffPalette, PaletteMode};
use shelbi_app::diff::pr::review_title;
use shelbi_app::diff::{Mode, PrTone};
use shelbi_state::keymap::{Key, KeyChord, Mods};

use super::{diff_keys, DiffView, STATUS_H};
use crate::elements::terminal::TerminalSettings;
use crate::theme::style::{self, Type, Typography};
use crate::theme::Theme;

/// The palette's width, and how far down it floats.
const PALETTE_W: f32 = 640.0;
const OVERLAY_TOP: f32 = 56.0;
/// The most palette rows on show at once.
const PALETTE_ROWS: usize = 12;
/// A palette row's height.
const PALETTE_ROW_H: f32 = 30.0;
/// The help's width, and a row's height.
const HELP_W: f32 = 880.0;
const HELP_ROW_H: f32 = 22.0;
/// The help's columns: its sections, in the shared order.
const HELP_COLUMNS: [&[&str]; 3] = [&["Move"], &["Find", "View"], &["Fold", "Comment", "Revert", "Pull request", "Other"]];

/// A chord as the help and Skim's README spell it: `j`, `G`, `gg`, `zM`,
/// `⌃d`, `Space`, `⇧Tab`.
pub fn help_spelling(chord: &KeyChord) -> String {
    let mut out = chord.leader.map_or_else(String::new, |leader| help_spelling(&leader.chord()));
    let m = chord.mods;
    if m.contains(Mods::CONTROL) {
        out.push('⌃');
    }
    if m.contains(Mods::ALT) {
        out.push('⌥');
    }
    if m.contains(Mods::SUPER) {
        out.push('⌘');
    }
    let shift = m.contains(Mods::SHIFT);
    let key: String = match chord.code {
        Key::Char(' ') => "Space".into(),
        // A shifted letter is its capital.
        Key::Char(c) if shift => c.to_uppercase().to_string(),
        Key::Char(c) => c.to_string(),
        Key::BackTab => "⇧Tab".into(),
        Key::Tab => "Tab".into(),
        Key::Enter => "↵".into(),
        Key::Esc => "Esc".into(),
        Key::Up => "↑".into(),
        Key::Down => "↓".into(),
        Key::Left => "←".into(),
        Key::Right => "→".into(),
        Key::PageUp => "PgUp".into(),
        Key::PageDown => "PgDn".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
        Key::Backspace => "⌫".into(),
        Key::Delete => "⌦".into(),
        Key::Insert => "Insert".into(),
        Key::F(n) => format!("F{n}"),
    };
    if shift && !matches!(chord.code, Key::Char(_) | Key::BackTab) {
        out.push('⇧');
    }
    out.push_str(&key);
    out
}

/// The code font, for the bars that echo typed text.
fn code_font(cx: &gpui::App) -> SharedString {
    TerminalSettings::current(cx).font_family.into()
}

/// A caret between what's before and after it.
fn caret(color: Hsla) -> Div {
    div().flex_none().w(px(2.0)).h(px(16.0)).bg(color)
}

impl DiffView {
    /// The status bar: the shared line, or the search prompt being typed.
    pub(super) fn render_status(&self, theme: &Theme) -> Div {
        let state = self.state();
        let bar = div()
            .debug_selector(|| "diff-status".into())
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .h(px(STATUS_H))
            .px(px(12.0))
            .type_style(Type::XSmall)
            .text_color(theme.muted)
            .whitespace_nowrap()
            .overflow_hidden();
        if let Mode::Search { query } = state.mode() {
            return bar
                .child(
                    div()
                        .debug_selector(|| "diff-search-prompt".into())
                        .flex_none()
                        .text_color(theme.text_loud)
                        .child(format!("/{query}")),
                )
                .child(caret(theme.focus_ring))
                .child(div().flex_none().ml(px(8.0)).child("↵ search all files · Esc cancel"));
        }
        // A revert's question has to be read: it isn't the usual quiet line.
        if matches!(state.mode(), Mode::Confirm(_)) {
            return bar.child(
                div()
                    .debug_selector(|| "diff-revert-confirm".into())
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(theme.text_loud)
                    .child(state.status()),
            );
        }
        let mut line = state.status();
        // What's held for a key to come: `3`, `g`, `3g`.
        let held: String = [
            state.count().map(|n| n.to_string()),
            state.pending().map(|chord| super::overlays::help_spelling(&chord)),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !held.is_empty() {
            line = format!("{line} · {held}");
        }
        if state.loading() && !state.table().is_empty() {
            line = format!("{line} · reading…");
        }
        let trouble = state
            .error()
            .map(|e| format!("Couldn't refresh: {e}"))
            .or_else(|| self.session.store_error().map(|e| format!("Not saved: {e}")));
        bar.child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().child(line))
            .children(trouble.map(|t| {
                div()
                    .debug_selector(|| "diff-status-trouble".into())
                    .flex_none()
                    .max_w(px(420.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(theme.warning)
                    .child(t)
            }))
    }

    /// The comment editor, docked above the status bar while one's written.
    pub(super) fn render_draft(&self, theme: &Theme) -> Option<Div> {
        let Mode::Comment(draft) = self.state().mode() else { return None };
        let title = match &draft.target {
            DraftTarget::New(anchor) if anchor.start == anchor.end => format!("Comment on {} line {}", anchor.path, anchor.start),
            DraftTarget::New(anchor) => format!("Comment on {} lines {}–{}", anchor.path, anchor.start, anchor.end),
            DraftTarget::Edit { .. } => "Edit comment".to_string(),
            DraftTarget::Reply { .. } => "Reply".to_string(),
            DraftTarget::GithubNew(anchor) if anchor.start == anchor.end => {
                format!("GitHub comment on {} line {}", anchor.path, anchor.start)
            }
            DraftTarget::GithubNew(anchor) => format!("GitHub comment on {} lines {}–{}", anchor.path, anchor.start, anchor.end),
            DraftTarget::GithubReply { .. } => "Reply on GitHub".to_string(),
            DraftTarget::Review { event } => review_title(*event),
        };
        let reviewing = matches!(draft.target, DraftTarget::Review { .. });
        let hint = match (reviewing, draft.inserting) {
            (true, true) => "Tab verdict · ↵ submit · ⌃J new line · Esc motions",
            (true, false) => "Tab verdict · ↵ submit · i insert · Esc cancel",
            (false, true) => "↵ save · ⌃J new line · Esc motions",
            (false, false) => "↵ save · i insert · Esc cancel",
        };
        Some(
            div()
                .debug_selector(|| "diff-draft".into())
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(6.0))
                .max_h(px(180.0))
                .overflow_hidden()
                .px(px(12.0))
                .py(px(8.0))
                .bg(theme.background)
                .type_style(Type::XSmall)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .child(div().text_color(theme.text_loud).child(title))
                        .child(div().text_color(theme.muted).child(hint)),
                )
                .child(draft_lines(draft, theme)),
        )
    }

    /// The palette or the help, over the view.
    pub(super) fn render_overlay(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.state().mode() {
            Mode::Palette(palette) => Some(self.render_palette(palette, theme, cx)),
            Mode::Help { scroll } => Some(self.render_help(*scroll, theme, window, cx)),
            Mode::PrInfo { scroll } => Some(self.render_pr_info(*scroll, theme, window, cx)),
            _ => None,
        }
    }

    fn render_palette(&self, palette: &DiffPalette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let items = palette.items(self.state().changeset());
        let (first, shown) = window_of(palette.selected, items.len(), PALETTE_ROWS);
        let prompt = match palette.mode {
            PaletteMode::Commands => "❯ >",
            PaletteMode::Files | PaletteMode::Commits => "❯",
        };
        let placeholder = match palette.mode {
            PaletteMode::Files => "Go to a file (> for commands)",
            PaletteMode::Commands => "Run a command",
            PaletteMode::Commits => "Review one commit",
        };
        let query_empty = palette.query.is_empty();
        let query = div()
            .debug_selector(|| "diff-palette-query".into())
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .h(px(40.0))
            .px(px(14.0))
            .type_style(Type::Base)
            .child(div().flex_none().text_color(theme.muted).child(prompt))
            .when(!query_empty, |d| d.child(div().flex_none().text_color(theme.text_loud).child(palette.query.clone())))
            .child(caret(theme.theme_cyan))
            .when(query_empty, |d| d.child(div().flex_none().text_color(theme.muted).child(placeholder)));
        let rows = items.iter().enumerate().skip(first).take(shown).map(|(index, item)| {
            let selected = index == palette.selected;
            div()
                .id(("diff-palette-row", index))
                .debug_selector(move || format!("diff-palette-row-{index}"))
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .h(px(PALETTE_ROW_H))
                .px(px(10.0))
                .mx(px(4.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .type_style(Type::XSmall)
                .text_color(if selected { theme.text_loud } else { theme.foreground })
                .when(selected, |d| d.bg(theme.background_hover))
                .hover(|d| d.bg(theme.background))
                .child(
                    div()
                        .when(selected, |d| d.debug_selector(|| "diff-palette-selected".into()))
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(item.label.clone()),
                )
                .child(div().flex_none().text_color(theme.muted).child(item.detail.clone()))
                .on_click(cx.listener(move |this, _, window, cx| this.palette_clicked(index, window, cx)))
                .into_any_element()
        });
        let rows: Vec<AnyElement> = rows.collect();
        let none = rows.is_empty();
        centered(
            style::popover(div(), theme)
                .debug_selector(|| "diff-palette".into())
                .flex()
                .flex_col()
                .w(px(PALETTE_W))
                .max_w_full()
                .pb(px(6.0))
                .overflow_hidden()
                .child(query)
                .child(div().flex_none().h(px(1.0)).bg(theme.divider))
                .when(none, |d| {
                    d.child(
                        div()
                            .debug_selector(|| "diff-palette-none".into())
                            .h(px(PALETTE_ROW_H))
                            .px(px(14.0))
                            .flex()
                            .items_center()
                            .type_style(Type::XSmall)
                            .text_color(theme.muted)
                            .child("Nothing matches"),
                    )
                })
                .children(rows),
            cx,
        )
        .into_any_element()
    }

    /// The `i` panel: every check and review, a row at a time.
    fn render_pr_info(&self, scroll: usize, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.state().pr_info_rows();
        let visible = pr_info_rows_fit(window);
        let room = visible as f32 * HELP_ROW_H;
        let first = scroll.min(rows.len().saturating_sub(visible));
        let shown = rows.into_iter().enumerate().skip(first).take(visible).map(|(index, row)| {
            let spans = row.into_iter().map(|span| {
                let color = pr_tone_color(span.tone, theme);
                div().flex_none().text_color(color).child(span.text)
            });
            div()
                .debug_selector(move || format!("diff-pr-info-row-{index}"))
                .flex()
                .flex_row()
                .items_center()
                .h(px(HELP_ROW_H))
                .overflow_hidden()
                .whitespace_nowrap()
                .children(spans)
        });
        centered(
            style::popover(div(), theme)
                .debug_selector(|| "diff-pr-info".into())
                .flex()
                .flex_col()
                .w(px(PALETTE_W))
                .max_w_full()
                .px(px(16.0))
                .py(px(14.0))
                .gap(px(10.0))
                .type_style(Type::XSmall)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child("Pull request"))
                        .child(div().text_color(theme.muted).child("j / k scroll · r read again · i close")),
                )
                .child(div().flex().flex_col().max_h(px(room)).overflow_hidden().children(shown)),
            cx,
        )
        .into_any_element()
    }

    fn render_help(&self, scroll: usize, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let keys = diff_keys(cx);
        let sections = help_sections(&keys, self.state().kind(), help_spelling);
        let family = code_font(cx);
        // The rows that fit under the title; `j` / `k` move the columns past.
        let room = (f32::from(window.viewport_size().height) - OVERLAY_TOP * 2.0 - 80.0).max(HELP_ROW_H);
        let longest = HELP_COLUMNS
            .iter()
            .map(|titles| {
                sections
                    .iter()
                    .filter(|s| titles.contains(&s.title))
                    .map(|s| s.rows.len() + 2)
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(0);
        let visible = (room / HELP_ROW_H) as usize;
        let scroll = scroll.min(longest.saturating_sub(visible));
        let columns = HELP_COLUMNS.iter().map(|titles| {
            let parts = sections.iter().filter(|s| titles.contains(&s.title)).map(|section| {
                let rows = section.rows.iter().map(|row| {
                    let action = row.action;
                    let label = row.label.to_string();
                    let keys = if row.keys.is_empty() { "unbound".to_string() } else { row.keys.join("  ") };
                    div()
                        .debug_selector(move || format!("diff-help-{action:?}"))
                        .flex()
                        .flex_row()
                        .justify_between()
                        .items_center()
                        .gap(px(12.0))
                        .h(px(HELP_ROW_H))
                        .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
                        .child(
                            div()
                                .flex_none()
                                .font_family(family.clone())
                                .text_color(if row.keys.is_empty() { theme.muted } else { theme.text_loud })
                                .child(keys),
                        )
                });
                div()
                    .flex()
                    .flex_col()
                    .pb(px(HELP_ROW_H))
                    .child(
                        div()
                            .h(px(HELP_ROW_H))
                            .type_style(Type::XSmallSemibold)
                            .text_color(theme.muted)
                            .child(section.title),
                    )
                    .children(rows)
            });
            div().flex().flex_col().flex_1().min_w_0().children(parts)
        });
        centered(
            style::popover(div(), theme)
                .debug_selector(|| "diff-help".into())
                .flex()
                .flex_col()
                .w(px(HELP_W))
                .max_w_full()
                .px(px(16.0))
                .py(px(14.0))
                .gap(px(10.0))
                .type_style(Type::XSmall)
                .text_color(theme.foreground)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child("Diff keys"))
                        .child(div().text_color(theme.muted).child("j / k scroll · any other key closes")),
                )
                .child(
                    div().max_h(px(room)).overflow_hidden().child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(20.0))
                            .mt(px(-(scroll as f32) * HELP_ROW_H))
                            .children(columns),
                    ),
                ),
            cx,
        )
        .into_any_element()
    }
}

/// How many rows the `i` panel has room for under its title.
pub(super) fn pr_info_rows_fit(window: &Window) -> usize {
    let room = (f32::from(window.viewport_size().height) - OVERLAY_TOP * 2.0 - 80.0).max(HELP_ROW_H);
    (room / HELP_ROW_H) as usize
}

/// The color a pull request's words read in, over a popover.
fn pr_tone_color(tone: PrTone, theme: &Theme) -> Hsla {
    match tone {
        PrTone::Normal => theme.foreground,
        PrTone::Loud => theme.text_loud,
        PrTone::Muted => theme.muted,
        PrTone::Good => theme.success,
        PrTone::Bad => theme.error,
        PrTone::Pending => theme.warning,
    }
}

/// `draft`'s lines, with the caret in its place.
fn draft_lines(draft: &CommentDraft, theme: &Theme) -> Div {
    let (row, col) = draft.area.cursor();
    let lines = draft.area.lines().iter().enumerate().map(|(at, chars)| {
        let line = div().flex().flex_row().items_center().h(px(16.0)).min_w_0().text_color(theme.foreground);
        if at != row {
            return line.child(chars.iter().collect::<String>());
        }
        let col = col.min(chars.len());
        line.child(chars[..col].iter().collect::<String>())
            .child(caret(theme.focus_ring))
            .child(chars[col..].iter().collect::<String>())
    });
    div().flex().flex_col().min_h_0().children(lines)
}

/// `el`, floated at the top of the view. A click off it is the view's
/// `on_outside`; one on it stays there.
fn centered(el: Div, cx: &mut Context<DiffView>) -> gpui::Stateful<Div> {
    div()
        .id("diff-overlay")
        .debug_selector(|| "diff-overlay".into())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .occlude()
        .flex()
        .flex_col()
        .items_center()
        .pt(px(OVERLAY_TOP))
        .px(px(16.0))
        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.dismiss_overlay(cx)))
        .child(el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()))
}

/// Which `max` of `len` rows to show so `selected` is among them: the first
/// shown, and how many.
pub fn window_of(selected: usize, len: usize, max: usize) -> (usize, usize) {
    let shown = len.min(max);
    let first = (selected + 1).saturating_sub(shown).min(len - shown);
    (first, shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_state::keymap::validate_keymaps_yaml;

    #[test]
    fn the_help_spells_keys_as_skim_does() {
        let km = validate_keymaps_yaml("", None).0.diff;
        let keys = |action| km.chords_for(action).into_iter().map(help_spelling).collect::<Vec<_>>();
        use shelbi_app::diff::DiffAction as A;
        assert!(keys(A::CursorDown).contains(&"j".to_string()));
        assert_eq!(keys(A::Bottom), ["G"]);
        assert_eq!(keys(A::Top), ["gg"]);
        assert_eq!(keys(A::NextChange), ["]h"]);
        assert_eq!(keys(A::CloseAllFolds), ["zM"]);
        assert_eq!(keys(A::SendToAgent), ["gY"]);
        assert_eq!(keys(A::PrevHunkFilter), ["⇧Tab"]);
        assert!(keys(A::PageDown).contains(&"Space".to_string()));
        assert!(keys(A::HalfPageDown).contains(&"⌃d".to_string()));
    }

    #[test]
    fn the_palette_window_keeps_the_selection_in_it() {
        assert_eq!(window_of(0, 3, 12), (0, 3));
        assert_eq!(window_of(0, 30, 12), (0, 12));
        assert_eq!(window_of(11, 30, 12), (0, 12));
        assert_eq!(window_of(12, 30, 12), (1, 12));
        assert_eq!(window_of(29, 30, 12), (18, 12));
        assert_eq!(window_of(0, 0, 12), (0, 0));
    }
}
