//! Shared rendering primitives for the **sidebar-column panels** — the review
//! panel ([`crate::review_panel`]) and the workspace panel
//! ([`crate::workspace_panel`]). Both replace the nav sidebar with a dedicated
//! column that shows, for the task it was opened on:
//!
//! - a **square back button** at the top (a back-arrow glyph drawn as a square
//!   via the sidebar nav's half-block bleed trick), with a **status label**
//!   beside it,
//! - **task info**: the title (bold white) and a few wrapped, markdown-stripped
//!   lines of the body preview, with a cyan **`More`** link that opens the full
//!   task-description popover,
//! - the worktree **folder** row (left-truncated, revealed on click), and
//! - a **nav block** of view switches, drawn exactly like the main sidebar's nav
//!   (full-width selection fill with the half-block bleed above/below).
//!
//! Everything here is pure (no `self` of either panel type) so both panels share
//! one implementation and the differences — the status label/colour, the switch
//! items, and whether an action row follows — stay in the panel types. This is
//! the "factor the shared header, task-info and worktree rendering into one
//! module used by both panels" the workspace-sidebar task asks for.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::sidebar::{BLEED_ABOVE, BLEED_BELOW};
use crate::theme::palette;

/// Outer left padding (in columns) for the header block, task info, worktree,
/// and actions — 2 cols per the Figma. The nav switches keep the sidebar's
/// narrower 1-col inset instead (see [`nav_block_line`]).
pub(crate) const PAD: u16 = 2;

/// Right gutter (in columns) kept clear between flowing text content and the
/// sidebar's right border, matching the left [`PAD`]. The task title, wrapped
/// description, `More` link, and the worktree path all wrap and ellipsize
/// within the content area **minus** this gutter, so no glyph lands against the
/// border. Only text content observes it — the nav switches keep their
/// edge-to-edge (1-col-inset) selection fill (see [`render_nav_block`]).
pub(crate) const PAD_RIGHT: u16 = PAD;

/// The square back button occupies three rendered lines: a lower-half-block
/// bleed row, the button (arrow) row, and an upper-half-block bleed row.
pub(crate) const BACK_BLOCK_H: u16 = 3;

/// The button (arrow) + status sit on the middle line of the three-line block.
/// Only the render tests anchor to it.
#[cfg(test)]
pub(crate) const BACK_BUTTON_LINE: u16 = 1;

/// Column width of the back button. A cell is ~twice as tall as it is wide and
/// the half-block bleed makes the button ~2 cells tall, so a handful of columns
/// reads as roughly square; the arrow is centered within it.
pub(crate) const BACK_BTN_WIDTH: usize = 5;

/// Most lines the task description preview may claim.
pub(crate) const MAX_DESC_LINES: usize = 3;

/// Max rows the bottom status/error line may claim.
const STATUS_MAX_H: u16 = 6;

/// 1-col horizontal inset the bottom status line renders into.
const STATUS_INDENT: ratatui::layout::Margin = ratatui::layout::Margin {
    horizontal: 1,
    vertical: 0,
};

/// The folder emoji + space that prefixes the worktree row. Named so the budget
/// math and the rendered line can never drift.
const FOLDER_PREFIX: &str = "📁 ";

/// Display column width of `s`, honoring wide glyphs.
pub(crate) fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Whether (`x`, `y`) falls inside `r`.
pub(crate) fn in_rect(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0
        && r.height > 0
        && x >= r.x
        && x < r.x.saturating_add(r.width)
        && y >= r.y
        && y < r.y.saturating_add(r.height)
}

/// Left-truncate `path` to at most `width` columns, prefixing `...` when it's
/// clipped, so the tail (the folder name) always stays visible:
/// `/a/b/c/.shelbi/wt/alpha` → `...wt/alpha`.
pub(crate) fn truncate_left(path: &str, width: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= width {
        return path.to_string();
    }
    if width <= 3 {
        return chars[chars.len().saturating_sub(width)..].iter().collect();
    }
    let keep = width - 3;
    let tail: String = chars[chars.len() - keep..].iter().collect();
    format!("...{tail}")
}

/// Right-truncate `s` to at most `width` columns, appending `…` when clipped.
/// Used for the task title, which the design keeps to one line.
pub(crate) fn truncate_right(s: &str, width: usize) -> String {
    if display_width(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    // Reserve one column for the ellipsis.
    let budget = width.saturating_sub(1);
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > budget {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// `label` with `lead` spaces before it and `trail` after, all in `style`
/// but its underline: that stays on the label's own cells, so a hovered
/// link's or a focused label's underline never runs into the padding
/// around it (a focus fill still does).
pub(crate) fn padded_label(label: impl Into<String>, lead: usize, trail: usize, style: Style) -> [Span<'static>; 3] {
    let pad = style.remove_modifier(Modifier::UNDERLINED);
    [
        Span::styled(" ".repeat(lead), pad),
        Span::styled(label.into(), style),
        Span::styled(" ".repeat(trail), pad),
    ]
}

/// Compose the worktree folder row — `"📁 <path>"` — fitting the whole line
/// within `width` columns. The prefix's display width is reserved *before*
/// left-truncating the path so the composed line never overruns the row.
pub(crate) fn folder_row_text(worktree: &str, width: usize) -> String {
    let budget = width.saturating_sub(display_width(FOLDER_PREFIX)).max(1);
    let label = truncate_left(worktree, budget);
    format!("{FOLDER_PREFIX}{label}")
}

/// Carve the bottom rows out of `area` for the status line when `status` is
/// non-empty. Returns `(body, Some(status_rect))`.
pub(crate) fn reserve_status_area(status: &str, area: Rect) -> (Rect, Option<Rect>) {
    if status.is_empty() || area.height < 2 {
        return (area, None);
    }
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let needed = wrapped_line_count(status, inner_w) as u16;
    let cap = STATUS_MAX_H.min(area.height - 1);
    let h = needed.clamp(1, cap);
    let body = Rect {
        height: area.height - h,
        ..area
    };
    let status_rect = Rect {
        y: area.y + area.height - h,
        height: h,
        ..area
    };
    (body, Some(status_rect))
}

/// Render a non-empty `status` line painted red and word-wrapped (an action
/// failure, an opener error).
pub(crate) fn render_status_line(f: &mut Frame, status: &str, area: Rect) {
    if area.width == 0 || area.height == 0 || status.is_empty() {
        return;
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            status.to_string(),
            Style::default().fg(palette().ansi_red),
        )))
        .wrap(ratatui::widgets::Wrap { trim: true }),
        area.inner(STATUS_INDENT),
    );
}

/// Estimate how many rows `text` occupies when word-wrapped to `width` columns,
/// matching ratatui's `Wrap { trim: true }` closely enough to size the status
/// area.
pub(crate) fn wrapped_line_count(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let mut lines = 1usize;
    let mut col = 0usize;
    for word in text.split_whitespace() {
        let w = display_width(word);
        if w > width {
            if col > 0 {
                lines += 1;
            }
            lines += (w - 1) / width;
            col = w % width;
            if col == 0 {
                col = width;
            }
            continue;
        }
        let need = if col == 0 { w } else { col + 1 + w };
        if need > width {
            lines += 1;
            col = w;
        } else {
            col = need;
        }
    }
    lines.max(1)
}

/// Advance the layout cursor one blank line, never past `bottom`.
pub(crate) fn advance_blank(y: u16, bottom: u16) -> u16 {
    (y + 1).min(bottom)
}

/// Columns a square button holding `glyph` takes: [`BACK_BTN_WIDTH`], or the
/// glyph with a column each side when that is wider (a word in place of an
/// emoji the terminal can't draw).
pub(crate) fn square_button_width(glyph: &str) -> usize {
    BACK_BTN_WIDTH.max(display_width(glyph) + 2)
}

/// A square button's glyph style on its fill: gray at rest, white and bold
/// when selected or focused.
pub(crate) fn square_glyph_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(palette().ansi_white)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette().ansi_gray)
    }
}

/// Draw a square button `width` columns wide from column `x`: a `▄` cap on
/// row `rows[0]`, `glyph` centered on the solid `fill` on row `rows[1]` (the
/// padding measured in cells, so a double-width emoji stays centered), and a
/// `▀` cap on row `rows[2]`. The caps carry the fill as their foreground on
/// the terminal's background, so the one glyph row reads as a square (the
/// sidebar nav's half-block trick). A row that is `None` (squeezed out) is
/// left alone. The back button and the sidebar footer's buttons both draw
/// through here.
pub(crate) fn draw_square_button(
    buf: &mut Buffer,
    x: u16,
    rows: [Option<u16>; 3],
    width: u16,
    glyph: &str,
    fill: Color,
    glyph_style: Style,
) {
    if width == 0 {
        return;
    }
    let w = width as usize;
    let cap = Style::default().fg(fill);
    if let Some(y) = rows[0] {
        buf.set_stringn(x, y, BLEED_ABOVE.repeat(w), w, cap);
    }
    if let Some(y) = rows[1] {
        let left = w.saturating_sub(display_width(glyph)).div_ceil(2);
        let face = format!("{}{glyph}", " ".repeat(left));
        buf.set_stringn(x, y, " ".repeat(w), w, glyph_style.bg(fill));
        buf.set_stringn(x, y, face, w, glyph_style.bg(fill));
        // A double-width glyph resets its continuation cell as it is laid
        // down, so re-fill every cell to keep the row one solid bar.
        for col in x..x + width {
            buf[(col, y)].set_bg(fill);
        }
    }
    if let Some(y) = rows[2] {
        buf.set_stringn(x, y, BLEED_BELOW.repeat(w), w, cap);
    }
}

/// Render the square back button block ([`draw_square_button`], a `←` on the
/// selection fill) and a `status` label beside it: on the button's own row,
/// two columns to its right, in `status_style`.
pub(crate) fn render_back_button(
    f: &mut Frame,
    area: Rect,
    selected: bool,
    status: &str,
    status_style: Style,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let avail = area.width.saturating_sub(PAD) as usize;
    let btn_w = BACK_BTN_WIDTH.min(avail.max(1)).max(1) as u16;
    let x = area.x + PAD.min(area.width - 1);
    let rows = [0, 1, 2].map(|i| (i < area.height).then(|| area.y + i));
    let buf = f.buffer_mut();
    draw_square_button(
        buf,
        x,
        rows,
        btn_w.min(area.right() - x),
        "\u{2190}",
        palette().selection,
        square_glyph_style(selected),
    );
    if let Some(y) = rows[1] {
        let status_x = x + btn_w + 2;
        if status_x < area.right() {
            let room = (area.right() - status_x) as usize;
            buf.set_stringn(status_x, y, status, room, status_style);
        }
    }
}

/// Render the task-info block (title, description preview, `More`) into `area`,
/// returning `(lines_used, more_hit)` so the caller can advance past it and
/// record the `More` link's click rect. A bodyless task shows just the title and
/// no `More` (the returned `more_hit` is `None`).
///
/// `more_selected` marks the `More` link as the keyboard-focused row: it then
/// renders with the selection fill behind it (white + bold), the same focus
/// state the nav block gives its selected item, rather than its resting cyan.
pub(crate) fn render_task_info(
    f: &mut Frame,
    area: Rect,
    title: &str,
    description: &str,
    more_selected: bool,
) -> (u16, Option<Rect>) {
    if area.width == 0 || area.height == 0 {
        return (0, None);
    }
    let inner_x = area.x + PAD;
    // Wrap and ellipsize against the content area minus both gutters so the text
    // never lands against the right border; the render rect below is this same
    // reduced width, keeping the two right-most columns blank.
    let width = area.width.saturating_sub(PAD + PAD_RIGHT) as usize;
    if width == 0 {
        return (0, None);
    }
    let mut lines: Vec<Line> = Vec::new();
    let mut used: u16 = 0;
    let mut more_hit = None;

    // Title (bold white, one line, right-truncated with …).
    if !title.is_empty() && used < area.height {
        lines.push(Line::from(Span::styled(
            truncate_right(title, width),
            Style::default().fg(palette().ansi_white).add_modifier(Modifier::BOLD),
        )));
        used += 1;
    }

    // Description preview (#c6c6c6, up to MAX_DESC_LINES, ending in `...` when
    // clipped). A bodyless task shows no preview and no More.
    let desc_lines = if description.trim().is_empty() {
        Vec::new()
    } else {
        description_preview(description, width, MAX_DESC_LINES)
    };
    for dl in &desc_lines {
        if used >= area.height {
            break;
        }
        lines.push(Line::from(Span::styled(
            dl.clone(),
            Style::default().fg(palette().foreground),
        )));
        used += 1;
    }

    // More link, only when a description was shown. Resting it is cyan; focused
    // it carries the selection fill (white + bold), matching the nav block's
    // selected item so keyboard focus reads consistently across the panel.
    if !desc_lines.is_empty() && used < area.height {
        let more_style = if more_selected {
            Style::default()
                .fg(palette().ansi_white)
                .bg(palette().selection)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette().cyan)
        };
        lines.push(Line::from(Span::styled("More", more_style)));
        more_hit = Some(Rect {
            x: inner_x,
            y: area.y + used,
            width: 4, // "More"
            height: 1,
        });
        used += 1;
    }

    f.render_widget(
        Paragraph::new(lines),
        Rect {
            x: inner_x,
            y: area.y,
            width: width as u16,
            height: used,
        },
    );
    (used, more_hit)
}

/// Render the worktree folder row: `📁 <path>`, left-truncated. `#bababa` when
/// unselected, white/bold when selected.
pub(crate) fn render_folder(f: &mut Frame, area: Rect, worktree: &str, selected: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // Left-truncate the path against the content area minus both gutters so the
    // tail ends two columns before the border (never clipped at it).
    let width = area.width.saturating_sub(PAD + PAD_RIGHT) as usize;
    let style = if selected {
        Style::default().fg(palette().ansi_white).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette().gray)
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            folder_row_text(worktree, width),
            style,
        ))),
        Rect {
            x: area.x + PAD,
            y: area.y,
            width: width as u16,
            height: 1,
        },
    );
}

/// One entry of a nav block: its glyph, label, and whether it is the *active*
/// (currently-shown) view — the active-but-unselected entry renders cyan/bold
/// like the main sidebar's active nav item.
pub(crate) struct NavEntry {
    pub glyph: &'static str,
    pub label: String,
    pub active: bool,
}

/// Render `entries` as a nav block, mirroring the main sidebar's nav exactly: a
/// separator line between (and bracketing) each item, the selected item's fill
/// **inset one column from each edge** with its adjacent separators carrying the
/// half-block bleed over that same inset region. The one-column gutters stay on
/// the terminal's default background. `selected` is the index into `entries` of
/// the selected item, if any. Returns each item's screen rect (full width, for
/// the caller's click map — a click anywhere on the row selects it).
pub(crate) fn render_nav_block(
    f: &mut Frame,
    area: Rect,
    entries: &[NavEntry],
    selected: Option<usize>,
) -> Vec<Rect> {
    let mut rects = Vec::with_capacity(entries.len());
    if area.width == 0 || area.height == 0 {
        return rects;
    }
    // Inset the fill, bleed, and label one column from each side — the same
    // `LIST_INDENT` the main sidebar's nav uses, so the selection block reads as
    // the same width and lines its icon up at col 2 (gutter + 1-col pad).
    let inner = area.inner(NAV_INSET);
    if inner.width == 0 {
        return rects;
    }
    let width = inner.width as usize;
    let bleed = palette().selection;

    // The item line for entry `p` sits at nav-area offset `2p + 1`.
    let content: Vec<Line> = entries
        .iter()
        .enumerate()
        .map(|(p, entry)| nav_block_line(entry, selected == Some(p), width, bleed))
        .collect();
    for p in 0..entries.len() {
        rects.push(Rect {
            x: area.x,
            y: area.y + (2 * p as u16 + 1),
            width: area.width,
            height: 1,
        });
    }
    let lines = crate::sidebar::bleed_block_lines(content, selected, width, bleed);
    f.render_widget(Paragraph::new(lines), inner);
    rects
}

/// 1-col horizontal inset for the nav block — matches the main sidebar's
/// `LIST_INDENT` so the panel nav reads identically.
const NAV_INSET: ratatui::layout::Margin = ratatui::layout::Margin {
    horizontal: 1,
    vertical: 0,
};

/// One nav block row. Selected rows fill the inset width with the selection
/// background and render white/bold; the active view is cyan/bold when
/// unselected; other rows are `#bababa`. The single leading space keeps the icon
/// aligned at col 2 (inset gutter + this 1-col pad), matching the main sidebar.
fn nav_block_line(entry: &NavEntry, selected: bool, width: usize, bg: Color) -> Line<'static> {
    let text = format!(" {} {}", entry.glyph, entry.label);
    if selected {
        let pad = width.saturating_sub(text.chars().count());
        Line::from(Span::styled(
            format!("{text}{}", " ".repeat(pad)),
            Style::default()
                .fg(palette().ansi_white)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ))
    } else if entry.active {
        Line::from(Span::styled(
            text,
            Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(Span::styled(text, Style::default().fg(palette().gray)))
    }
}

// ---------------------------------------------------------------------------
// Description preview (markdown stripped, wrapped, clipped) — pure, unit-tested.

/// Build the task-description preview: strip block markdown (headings, list
/// markers, blockquotes, inline code backticks), flatten to a single
/// whitespace-separated stream, greedily wrap to `width` columns, and keep at
/// most `max_lines` lines — appending `...` to the last kept line when the body
/// runs past the preview. Returns an empty vec for an empty body or zero width.
pub(crate) fn description_preview(body: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    // The markdown stripping is the shared model's, so the desktop's preview
    // reads the same prose.
    let text = shelbi_app::review::description_text(body);
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }

    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut idx = 0;
    while idx < words.len() {
        let w = &words[idx];
        let need = if cur.is_empty() {
            display_width(w)
        } else {
            display_width(&cur) + 1 + display_width(w)
        };
        if need <= width {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(w);
            idx += 1;
        } else if cur.is_empty() {
            // A single word wider than the row: hard-truncate it onto this line.
            cur = truncate_to_width(w, width);
            idx += 1;
        } else {
            lines.push(std::mem::take(&mut cur));
            if lines.len() == max_lines {
                break;
            }
        }
    }
    if !cur.is_empty() && lines.len() < max_lines {
        lines.push(std::mem::take(&mut cur));
    }

    // Anything left over means the preview was clipped — mark the last line.
    if idx < words.len() {
        if let Some(last) = lines.last_mut() {
            append_ellipsis(last, width);
        }
    }
    lines
}

/// Truncate `s` to at most `width` columns (no ellipsis) — used when a single
/// word is wider than the whole row.
fn truncate_to_width(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

/// Trim `line` as needed and append `...` so the result fits within `width`.
fn append_ellipsis(line: &mut String, width: usize) {
    const ELL: &str = "...";
    if width < ELL.len() {
        return;
    }
    while display_width(line) + ELL.len() > width {
        if line.pop().is_none() {
            break;
        }
    }
    while line.ends_with(' ') {
        line.pop();
    }
    line.push_str(ELL);
}

// ---------------------------------------------------------------------------
// Platform open/reveal command builders (pure, unit-tested)

/// Host OS family for picking the file-manager / browser opener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    Macos,
    Windows,
    Linux,
}

/// The OS this binary was built for. Everything else is treated as Linux
/// (xdg-open), which is the correct default for the BSDs too.
pub fn current_os() -> OsKind {
    if cfg!(target_os = "macos") {
        OsKind::Macos
    } else if cfg!(target_os = "windows") {
        OsKind::Windows
    } else {
        OsKind::Linux
    }
}

/// Argv (`program`, `args`) that reveals `path` in the OS file manager:
/// `open` on macOS, `explorer` on Windows, `xdg-open` on Linux.
pub fn reveal_command(os: OsKind, path: &str) -> (String, Vec<String>) {
    let program = match os {
        OsKind::Macos => "open",
        OsKind::Windows => "explorer",
        OsKind::Linux => "xdg-open",
    };
    (program.to_string(), vec![path.to_string()])
}

/// Argv (`program`, `args`) that opens `url` in the system browser — same
/// per-platform opener as [`reveal_command`].
pub fn open_url_command(os: OsKind, url: &str) -> (String, Vec<String>) {
    let program = match os {
        OsKind::Macos => "open",
        OsKind::Windows => "explorer",
        OsKind::Linux => "xdg-open",
    };
    (program.to_string(), vec![url.to_string()])
}

/// Run a fire-and-forget opener command, mapping a launch failure to a short
/// message the caller can surface on the status line (never a crash).
pub(crate) fn spawn_opener(program: &str, args: &[String]) -> std::result::Result<(), String> {
    std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{program} failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_left_prefixes_ellipsis_and_keeps_the_tail() {
        assert_eq!(truncate_left("/a/b/c/.shelbi/wt/alpha", 11), "...wt/alpha");
        assert_eq!(truncate_left("short", 20), "short");
        assert!(truncate_left("/very/long/path/here", 12).starts_with("..."));
        assert!(truncate_left("/very/long/path/here", 12).ends_with("path/here"));
    }

    #[test]
    fn folder_row_reserves_the_emoji_prefix_before_truncating() {
        let worktree = "/Users/jlong/Workspaces/32pixels/ContextStore/.shelbi/wt/alpha";
        for width in [16_usize, 20, 24, 30, 40] {
            let line = folder_row_text(worktree, width);
            assert!(
                display_width(&line) <= width,
                "line {line:?} (w={}) overruns row width {width}",
                display_width(&line),
            );
            assert!(line.ends_with("wt/alpha"), "line {line:?} lost the tail at width {width}");
            assert!(line.starts_with(FOLDER_PREFIX));
        }
    }

    #[test]
    fn description_preview_strips_markdown_wraps_and_clips() {
        let body = "## Summary\n\nWarm the application cache during startup so the first request can reuse the same data as subsequent requests.\n\n## Acceptance\n\n- Initialize the cache once.";
        let out = description_preview(body, 40, 3);
        assert_eq!(out.len(), 3, "at most three lines: {out:?}");
        assert!(out[0].starts_with("Warm the application cache"), "stripped heading: {out:?}");
        assert!(!out.join(" ").contains('#'), "no heading markers: {out:?}");
        assert!(out[2].ends_with("..."), "clipped ends in ...: {out:?}");
        for line in &out {
            assert!(display_width(line) <= 40);
        }
    }

    #[test]
    fn description_preview_of_a_short_body_is_not_ellipsized() {
        assert_eq!(description_preview("A tiny note.", 40, 3), vec!["A tiny note.".to_string()]);
    }

    #[test]
    fn description_preview_strips_list_markers() {
        assert_eq!(
            description_preview("- first item\n- second item", 40, 3),
            vec!["first item second item".to_string()]
        );
    }

    #[test]
    fn wrapped_line_count_matches_greedy_word_wrap() {
        assert_eq!(wrapped_line_count("", 10), 1);
        assert_eq!(wrapped_line_count("short", 10), 1);
        assert_eq!(wrapped_line_count("hello world", 8), 2);
        assert_eq!(wrapped_line_count("abcdefghij", 4), 3);
    }

    #[test]
    fn reveal_and_open_commands_are_per_platform() {
        assert_eq!(reveal_command(OsKind::Macos, "/p"), ("open".into(), vec!["/p".into()]));
        assert_eq!(reveal_command(OsKind::Linux, "/p"), ("xdg-open".into(), vec!["/p".into()]));
        assert_eq!(
            reveal_command(OsKind::Windows, "C:\\p"),
            ("explorer".into(), vec!["C:\\p".into()])
        );
        assert_eq!(
            open_url_command(OsKind::Macos, "https://x"),
            ("open".into(), vec!["https://x".into()])
        );
    }
}
