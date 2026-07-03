//! Inline-markdown rendering for the note bodies shown in the review and
//! kanban panes.
//!
//! Task and review notes are stored as plain markdown strings. The one
//! inline construct we surface visually is backtick `code`, painted with a
//! subtle background so it reads as code against the surrounding prose.
//!
//! We deliberately do **not** lean on `Paragraph`'s built-in `Wrap` for
//! this. ratatui word-wraps *after* the spans are styled, and a
//! background-styled span that lands at the wrap boundary paints its
//! background across the trailing padding to the paragraph's edge — so an
//! inline-code highlight sitting at the right edge of a wrapped line bleeds
//! past the pane's content area into the margin/border. To keep the
//! highlight clamped to the glyphs it covers we word-wrap here to a known
//! `width` and hand the caller pre-wrapped `Line`s (rendered *without*
//! `.wrap()`), so a code span never crosses the boundary and its background
//! only ever covers characters inside `width`.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Background tint for inline `code` spans. `Indexed(236)` is a dark gray
/// that sits just above the default terminal background — the same tint the
/// activity feed uses for machine-driven rows.
const CODE_BG: Color = Color::Indexed(236);
/// Foreground for inline code — a light gray that stays legible on the
/// tint without reading as a hard white box.
const CODE_FG: Color = Color::Indexed(252);

fn code_style() -> Style {
    Style::default().fg(CODE_FG).bg(CODE_BG)
}

/// Render a markdown note `body` into pre-wrapped lines for a pane of the
/// given inner `width` (in cells). Backtick spans become highlighted inline
/// code; everything else is plain text. Wrapping happens here so the code
/// highlight never overflows the pane's right edge.
///
/// A `width` of 0 disables wrapping (each source line becomes one output
/// line) — callers only hit this during a zero-area draw.
pub fn render_note(body: &str, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    // `split('\n')` keeps blank lines (and the empty tail of a trailing
    // newline) so the note's vertical spacing is preserved.
    for src in body.split('\n') {
        let runs = parse_runs(src);
        wrap_runs(runs, width, &mut out);
    }
    out
}

/// One styled slice of a word; `code` toggles the highlight.
struct Seg {
    text: String,
    code: bool,
}

/// A token in a source line: a run of whitespace (carrying its own code
/// flag so an inline-code highlight stays continuous across internal
/// spaces), or a word (a maximal run of non-whitespace, which may span a
/// code/plain boundary).
enum Tok {
    Space(Seg),
    Word(Vec<Seg>),
}

/// Split a source line into `(text, is_code)` runs on backtick pairs. An
/// unclosed trailing backtick is rendered literally so no text is lost.
fn parse_runs(line: &str) -> Vec<(String, bool)> {
    let pieces: Vec<&str> = line.split('`').collect();
    let last = pieces.len().saturating_sub(1);
    let mut runs: Vec<(String, bool)> = Vec::new();
    for (i, piece) in pieces.iter().enumerate() {
        let is_code_slot = i % 2 == 1;
        if is_code_slot && i != last {
            // A closing backtick followed this piece — real inline code.
            runs.push((piece.to_string(), true));
        } else if is_code_slot {
            // Opening backtick with no partner: keep the backtick as text.
            runs.push((format!("`{piece}"), false));
        } else {
            runs.push((piece.to_string(), false));
        }
    }
    runs
}

/// Break `runs` into whitespace / word tokens, merging adjacent code and
/// plain slices with no space between them into a single word so they wrap
/// as one unit (e.g. project-level `` `default_branch` ``).
fn tokenize(runs: Vec<(String, bool)>) -> Vec<Tok> {
    let mut toks: Vec<Tok> = Vec::new();
    let mut word: Vec<Seg> = Vec::new();
    for (text, code) in runs {
        for (is_space, chunk) in split_ws(&text) {
            if is_space {
                if !word.is_empty() {
                    toks.push(Tok::Word(std::mem::take(&mut word)));
                }
                toks.push(Tok::Space(Seg { text: chunk, code }));
            } else {
                word.push(Seg { text: chunk, code });
            }
        }
    }
    if !word.is_empty() {
        toks.push(Tok::Word(word));
    }
    toks
}

/// Split `s` into maximal whitespace / non-whitespace runs, each tagged
/// with whether it is whitespace.
fn split_ws(s: &str) -> Vec<(bool, String)> {
    let mut runs: Vec<(bool, String)> = Vec::new();
    let mut cur = String::new();
    let mut cur_space: Option<bool> = None;
    for ch in s.chars() {
        let sp = ch == ' ' || ch == '\t';
        match cur_space {
            Some(b) if b == sp => cur.push(ch),
            Some(b) => {
                runs.push((b, std::mem::take(&mut cur)));
                cur.push(ch);
                cur_space = Some(sp);
            }
            None => {
                cur.push(ch);
                cur_space = Some(sp);
            }
        }
    }
    if let Some(b) = cur_space {
        runs.push((b, cur));
    }
    runs
}

fn cell_width(s: &str) -> usize {
    s.chars().count()
}

/// Greedy word-wrap of one source line's tokens into `out`, always emitting
/// at least one line (blank lines are preserved).
fn wrap_runs(runs: Vec<(String, bool)>, width: usize, out: &mut Vec<Line<'static>>) {
    let toks = tokenize(runs);
    let start_len = out.len();

    if width == 0 {
        out.push(toks_to_line(&toks));
        return;
    }

    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    // Separator whitespace is held back until we know a word follows on the
    // same line, so trailing space is trimmed at a wrap and never painted.
    // Each segment keeps its code flag so a highlight spanning an internal
    // space stays continuous.
    let mut pending: Vec<Seg> = Vec::new();

    for tok in toks {
        match tok {
            Tok::Space(seg) => {
                if cur.is_empty() {
                    // Leading indent on a fresh line — keep it verbatim.
                    cur_w += cell_width(&seg.text);
                    push_seg(&mut cur, seg);
                } else {
                    pending.push(seg);
                }
            }
            Tok::Word(segs) => {
                let ww: usize = segs.iter().map(|s| cell_width(&s.text)).sum();
                let sep_w: usize = pending.iter().map(|s| cell_width(&s.text)).sum();
                if cur_w > 0 && cur_w + sep_w + ww > width {
                    out.push(Line::from(std::mem::take(&mut cur)));
                    cur_w = 0;
                    pending.clear();
                } else {
                    for seg in pending.drain(..) {
                        cur_w += cell_width(&seg.text);
                        push_seg(&mut cur, seg);
                    }
                }

                if ww <= width {
                    for seg in segs {
                        push_seg(&mut cur, seg);
                    }
                    cur_w += ww;
                } else {
                    // Word wider than the whole pane: hard-split it so the
                    // code highlight can't run past the right edge.
                    hard_split(segs, width, &mut cur, &mut cur_w, out);
                }
            }
        }
    }

    if !cur.is_empty() {
        out.push(Line::from(cur));
    } else if out.len() == start_len {
        // Blank source line — keep a blank output line for spacing.
        out.push(Line::from(""));
    }
}

/// Append a word wider than `width` a line at a time, coalescing same-style
/// runs into spans so the code highlight stays glyph-tight on every row.
fn hard_split(
    segs: Vec<Seg>,
    width: usize,
    cur: &mut Vec<Span<'static>>,
    cur_w: &mut usize,
    out: &mut Vec<Line<'static>>,
) {
    let chars: Vec<(char, bool)> = segs
        .iter()
        .flat_map(|s| s.text.chars().map(move |c| (c, s.code)))
        .collect();

    let mut i = 0;
    while i < chars.len() {
        if *cur_w >= width {
            out.push(Line::from(std::mem::take(cur)));
            *cur_w = 0;
        }
        let avail = width - *cur_w;
        let end = (i + avail).min(chars.len());
        let mut j = i;
        while j < end {
            let code = chars[j].1;
            let mut text = String::new();
            while j < end && chars[j].1 == code {
                text.push(chars[j].0);
                j += 1;
            }
            let style = if code { code_style() } else { Style::default() };
            cur.push(Span::styled(text, style));
        }
        *cur_w += end - i;
        i = end;
    }
}

fn push_seg(cur: &mut Vec<Span<'static>>, seg: Seg) {
    if seg.text.is_empty() {
        return;
    }
    let style = if seg.code {
        code_style()
    } else {
        Style::default()
    };
    cur.push(Span::styled(seg.text, style));
}

/// Flatten tokens onto a single unwrapped line (the `width == 0` fallback).
fn toks_to_line(toks: &[Tok]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for tok in toks {
        match tok {
            Tok::Space(seg) => {
                let style = if seg.code {
                    code_style()
                } else {
                    Style::default()
                };
                spans.push(Span::styled(seg.text.clone(), style));
            }
            Tok::Word(segs) => {
                for seg in segs {
                    if seg.text.is_empty() {
                        continue;
                    }
                    let style = if seg.code {
                        code_style()
                    } else {
                        Style::default()
                    };
                    spans.push(Span::styled(seg.text.clone(), style));
                }
            }
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten a line back to its plain text for content assertions.
    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Every span's visible width, summed — must never exceed the pane.
    fn line_width(line: &Line<'_>) -> usize {
        line.spans.iter().map(|s| cell_width(&s.content)).sum()
    }

    /// True if any span in the line carries the inline-code background.
    fn has_code_bg(line: &Line<'_>) -> bool {
        line.spans
            .iter()
            .any(|s| s.style.bg == Some(CODE_BG))
    }

    #[test]
    fn plain_text_has_no_code_style() {
        let lines = render_note("just some prose", 40);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "just some prose");
        assert!(!has_code_bg(&lines[0]));
    }

    #[test]
    fn inline_code_gets_highlight() {
        let lines = render_note("run `cargo test` now", 40);
        assert_eq!(line_text(&lines[0]), "run cargo test now");
        assert!(has_code_bg(&lines[0]));
        // Only the code word carries the background.
        let coded: String = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.bg == Some(CODE_BG))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(coded, "cargo test");
    }

    #[test]
    fn no_line_exceeds_width() {
        let note = "shelbi's rebase/probe `ignore` the workflow's base_branch \
                    and fall back to the project-level `default_branch`.";
        for width in [12usize, 18, 24, 30, 40] {
            let lines = render_note(note, width);
            for line in &lines {
                assert!(
                    line_width(line) <= width,
                    "line {:?} width {} exceeds {}",
                    line_text(line),
                    line_width(line),
                    width,
                );
            }
        }
    }

    #[test]
    fn code_span_at_wrap_boundary_stays_in_bounds() {
        // A width tuned so `ignore` lands at the right edge of a wrapped
        // line — the original overflow repro.
        let note = "the probe will ignore the `default_branch` here";
        for width in 10..=48 {
            let lines = render_note(note, width);
            for line in &lines {
                assert!(
                    line_width(line) <= width,
                    "width {width}: line {:?} = {} cells",
                    line_text(line),
                    line_width(line),
                );
                // A code span must never be the trailing padding past the
                // content: its glyphs are always within the measured width.
                if has_code_bg(line) {
                    assert!(line_width(line) <= width);
                }
            }
        }
    }

    #[test]
    fn word_longer_than_width_is_hard_split() {
        let lines = render_note("`supercalifragilisticexpialidocious`", 10);
        assert!(lines.len() > 1, "long code word should wrap across lines");
        for line in &lines {
            assert!(line_width(line) <= 10);
            assert!(has_code_bg(line), "each fragment keeps the code tint");
        }
        let joined: String = lines.iter().map(line_text).collect();
        assert_eq!(joined, "supercalifragilisticexpialidocious");
    }

    #[test]
    fn blank_lines_are_preserved() {
        let lines = render_note("first\n\nsecond", 20);
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "first");
        assert_eq!(line_text(&lines[1]), "");
        assert_eq!(line_text(&lines[2]), "second");
    }

    #[test]
    fn unclosed_backtick_is_literal() {
        let lines = render_note("a stray ` backtick", 40);
        assert_eq!(line_text(&lines[0]), "a stray ` backtick");
        assert!(!has_code_bg(&lines[0]));
    }

    #[test]
    fn leading_indent_is_kept() {
        let lines = render_note("    indented `code` line", 40);
        assert!(line_text(&lines[0]).starts_with("    indented"));
    }

    #[test]
    fn zero_width_does_not_panic() {
        let lines = render_note("some `code` text", 0);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "some code text");
    }
}
