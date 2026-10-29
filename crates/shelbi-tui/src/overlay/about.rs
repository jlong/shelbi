//! About Shelbi: the TUI's twin of the desktop's About window
//! (`desktop/src/views/about.rs`; Figma "About Shelbi · Dark", 2431:8310).
//!
//! The same structure, words and order, from [`shelbi_app::about`]: the app
//! icon, the name and this build's version, the tagline, the Website /
//! Documentation / GitHub links, the copyright line, and the license and
//! sponsor links. The desktop's window has its traffic lights to close it;
//! here a Close button sits at the bottom right, and Esc closes too.
//!
//! The panel is a dialog like every other: the search box's fill in the
//! dialogs' muted single-line border ([`super::dialog_frame`]). The icon is
//! pixel art from the desktop's icons ([`super::about_icon`]), two pixels to
//! a cell with `▀`: the one `desktop.app_icon` chooses (Command Grille
//! unless it says Retro), as the desktop's About shows. It is left out when
//! the window is too short for it.
//!
//! Tab (or an arrow) moves focus through the links and Close; Enter or
//! Space activates it; a click opens a link or closes. A link the pointer is
//! over is underlined, its label only (not the padding its focus fill takes). Acknowledgements opens the TUI's list of the bundled
//! software (`shell::acknowledgements`) in About's place; its Done brings
//! About back, focused on the link.

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use shelbi_app::about::{self, AboutLink};
use shelbi_state::AppIcon;
use unicode_width::UnicodeWidthStr;

use super::about_icon;
use crate::button::Button;
use crate::panel;
use crate::sidebar::BLEED_BELOW;
use crate::theme::{palette, pixel, tokens};

/// This build's version (the TUI's crate is the workspace's).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The panel's width when the window has room for it.
const WIDTH: u16 = 56;
/// Columns between the panel's sides and its content: the border and the
/// dialogs' padding inside it.
const PAD_X: u16 = 1 + super::DIALOG_PAD_X;
/// The Close button's label, as the TUI's dialogs name it.
const CLOSE: &str = "Close";
/// The button's columns: the label with two of fill each side.
const BUTTON_W: u16 = CLOSE.len() as u16 + 4;
/// The icon's rows: two pixels to a cell.
const ICON_ROWS: usize = about_icon::SIZE / 2;

/// Something focusable: a link, or the Close button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Link(AboutLink),
    Close,
}

/// What a key or click asks of the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Stay,
    Close,
    /// Open this page in the browser; the overlay stays up, as the
    /// desktop's window does.
    Open(&'static str),
    /// Show Acknowledgements (the bundled software) in About's place.
    Acknowledgements,
}

/// `icon`'s pixel art.
fn pixels(icon: AppIcon) -> &'static about_icon::Pixels {
    match icon {
        AppIcon::CommandGrille => &about_icon::COMMAND_GRILLE,
        AppIcon::Retro => &about_icon::RETRO,
    }
}

/// The icon `desktop.app_icon` chooses for About: Command Grille when it is
/// unset or `config.yaml` can't be read. A test never reads the real
/// `~/.shelbi`, so it gets Command Grille.
pub fn configured_icon() -> AppIcon {
    #[cfg(test)]
    {
        AppIcon::default()
    }
    #[cfg(not(test))]
    {
        shelbi_state::load_user_config()
            .map(|cfg| AppIcon::from_config(&cfg.desktop))
            .unwrap_or_default()
    }
}

/// The focus order: the links as they read, then Close.
fn targets() -> Vec<Target> {
    about::LINKS.into_iter().map(Target::Link).chain([Target::Close]).collect()
}

/// The overlay's state: what has focus, what the pointer is over, which
/// icon it draws, and whether the terminal draws 24-bit color (the icon
/// falls back to the xterm-256 palette when it doesn't).
#[derive(Debug, Clone)]
pub struct About {
    focus: Target,
    hover: Option<Target>,
    icon: AppIcon,
    truecolor: bool,
}

impl About {
    /// Opened with Close focused, so Enter dismisses it, drawing Command
    /// Grille (see [`About::with_icon`]).
    pub fn new(truecolor: bool) -> Self {
        About {
            focus: Target::Close,
            hover: None,
            icon: AppIcon::default(),
            truecolor,
        }
    }

    /// Draw `icon` (`desktop.app_icon`'s choice).
    pub fn with_icon(self, icon: AppIcon) -> Self {
        About { icon, ..self }
    }

    /// The icon it draws.
    pub fn icon(&self) -> AppIcon {
        self.icon
    }

    /// Opened again with `focus` focused (back from Acknowledgements, on
    /// its link).
    pub fn focused(truecolor: bool, focus: Target) -> Self {
        About {
            focus,
            ..About::new(truecolor)
        }
    }

    pub fn focus(&self) -> Target {
        self.focus
    }

    /// The panel's rect within `area` (the full window).
    pub fn rect(&self, area: Rect) -> Rect {
        layout(area).panel
    }

    pub fn handle_key(&mut self, ev: KeyEvent) -> Step {
        match ev.code {
            KeyCode::Esc => Step::Close,
            KeyCode::Tab | KeyCode::Right | KeyCode::Down => {
                self.step_focus(1);
                Step::Stay
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Up => {
                self.step_focus(-1);
                Step::Stay
            }
            KeyCode::Enter | KeyCode::Char(' ') => activate(self.focus),
            _ => Step::Stay,
        }
    }

    /// A mouse event over `area` (the full window): a move tracks the
    /// hovered target, a left click focuses and activates it.
    pub fn handle_mouse(&mut self, m: MouseEvent, area: Rect) -> Step {
        let hit = layout(area).hit(m.column, m.row);
        match m.kind {
            MouseEventKind::Moved => {
                self.hover = hit;
                Step::Stay
            }
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(target) => {
                    self.focus = target;
                    activate(target)
                }
                None => Step::Stay,
            },
            _ => Step::Stay,
        }
    }

    fn step_focus(&mut self, by: isize) {
        let order = targets();
        let at = order.iter().position(|t| *t == self.focus).unwrap_or(0) as isize;
        let n = order.len() as isize;
        self.focus = order[(at + by).rem_euclid(n) as usize];
    }

    /// Paint the panel into `buf`; `area` is the full window (the panel
    /// centers itself in it, as [`Self::rect`] says).
    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let plan = layout(area);
        let panel = plan.panel;
        if panel.width == 0 || panel.height == 0 {
            return;
        }
        super::dialog_frame(buf, panel, Some(palette().search), None);
        for (y, row) in &plan.rows {
            match row {
                Row::Gap | Row::Pad => {}
                Row::Icon(r) => self.render_icon_row(buf, panel, *y, *r),
                Row::Center(pieces) => {
                    let mut x = plan.content.x + plan.content.width.saturating_sub(pieces_width(pieces)) / 2;
                    for p in pieces {
                        let style = self.piece_style(p);
                        // A link's padding takes its focus fill but not its
                        // hover underline.
                        let pad = usize::from(p.target.is_some());
                        let label = &p.text[pad..p.text.len() - pad];
                        for span in panel::padded_label(label, pad, pad, style) {
                            let room = plan.content.right().saturating_sub(x);
                            x = buf.set_span(x, *y, &span, room).0;
                        }
                    }
                }
                Row::Button(part) => self.render_button_row(buf, plan.button_x(), *y, *part),
            }
        }
    }

    fn piece_style(&self, p: &Piece) -> Style {
        let base = Style::default().bg(palette().search);
        let style = match p.role {
            Role::Name => base.fg(palette().white).add_modifier(Modifier::BOLD),
            Role::Body => base.fg(palette().foreground),
            Role::Muted => base.fg(palette().muted),
            Role::Link => base.fg(palette().cyan),
            Role::MutedLink => base.fg(palette().muted),
        };
        match p.target {
            // The selection fill behind a focused link, as the review
            // panel's focused `More` link draws.
            Some(t) if t == self.focus => Style::default()
                .bg(palette().selection)
                .fg(palette().white)
                .add_modifier(Modifier::BOLD),
            Some(t) if Some(t) == self.hover => style.add_modifier(Modifier::UNDERLINED),
            _ => style,
        }
    }

    /// One row of the icon: each cell's `▀` is its top pixel, its
    /// background the bottom one, both blended over the panel's fill.
    fn render_icon_row(&self, buf: &mut Buffer, panel: Rect, y: u16, row: usize) {
        let size = about_icon::SIZE as u16;
        if panel.width < size {
            return;
        }
        let pixels = pixels(self.icon);
        let x0 = panel.x + (panel.width - size) / 2;
        for c in 0..about_icon::SIZE {
            let top = pixels[2 * row * about_icon::SIZE + c];
            let bottom = pixels[(2 * row + 1) * about_icon::SIZE + c];
            if top[3] == 0 && bottom[3] == 0 {
                continue;
            }
            let cell = &mut buf[(x0 + c as u16, y)];
            cell.set_symbol(BLEED_BELOW)
                .set_fg(self.art_color(top))
                .set_bg(self.art_color(bottom));
        }
    }

    /// An icon pixel over the panel's fill. A clear pixel is the fill
    /// itself (its exact color, which may be a palette slot), so the icon's
    /// corners never show a seam.
    fn art_color(&self, [r, g, b, a]: [u8; 4]) -> Color {
        if a == 0 {
            return palette().search;
        }
        let under = tokens().search.rgb;
        let mix = |c: u8, u: u8| ((c as u32 * a as u32 + u as u32 * (255 - a as u32) + 127) / 255) as u8;
        let (r, g, b) = (mix(r, under.r), mix(g, under.g), mix(b, under.b));
        pixel(r, g, b, self.truecolor)
    }

    /// The Close button as Settings' buttons draw (a Secondary button, its
    /// focus and its lift under the pointer), one of its three rows at a
    /// time, its caps over the panel's fill.
    fn render_button_row(&self, buf: &mut Buffer, x: u16, y: u16, part: ButtonPart) {
        let close = Button::secondary(CLOSE).focused(self.focus == Target::Close).hovered(self.hover == Some(Target::Close));
        let [above, label, below] = close.spans();
        let span = match part {
            ButtonPart::Top => above,
            ButtonPart::Label => label,
            ButtonPart::Bottom => below,
        };
        buf.set_span(x, y, &span, BUTTON_W);
        if part != ButtonPart::Label {
            for dx in 0..BUTTON_W {
                if let Some(cell) = buf.cell_mut((x + dx, y)) {
                    cell.set_bg(palette().search);
                }
            }
        }
    }
}

/// Enter / a click on `target`.
fn activate(target: Target) -> Step {
    match target {
        Target::Close => Step::Close,
        Target::Link(AboutLink::Acknowledgements) => Step::Acknowledgements,
        Target::Link(link) => link.url().map_or(Step::Stay, Step::Open),
    }
}

// ---------------------------------------------------------------------------
// Layout: one pure function of the window, shared by drawing and clicking.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonPart {
    Top,
    Label,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Name,
    Body,
    Muted,
    Link,
    MutedLink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    text: String,
    role: Role,
    target: Option<Target>,
}

impl Piece {
    fn text(text: impl Into<String>, role: Role) -> Self {
        Piece { text: text.into(), role, target: None }
    }

    /// A link, a column of padding each side so its focus fill reads as a
    /// pill without moving the row when focus moves. (The padding is a
    /// space each side, which drawing strips back off for the underline.)
    fn link(link: AboutLink, role: Role) -> Self {
        Piece {
            text: format!(" {} ", link.label()),
            role,
            target: Some(Target::Link(link)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    /// The panel's inner padding (top and bottom), the last blank to go.
    Pad,
    /// The space between groups (the desktop's 24pt gaps), dropped first
    /// when the window is short.
    Gap,
    /// Row `n` of the icon's cells.
    Icon(usize),
    /// Pieces centered across the content.
    Center(Vec<Piece>),
    Button(ButtonPart),
}

fn pieces_width(pieces: &[Piece]) -> u16 {
    pieces.iter().map(|p| p.text.width()).sum::<usize>() as u16
}

/// Lay `items` out in centered rows no wider than `width`, `sep` between
/// neighbors on a row (dropped where a row breaks).
fn pack(items: Vec<Piece>, sep: Option<Piece>, width: usize) -> Vec<Row> {
    let mut rows: Vec<Vec<Piece>> = Vec::new();
    let mut used = 0;
    for item in items {
        let w = item.text.width();
        let sep_w = sep.as_ref().map_or(0, |s| s.text.width());
        match rows.last_mut() {
            Some(row) if used + sep_w + w <= width => {
                if let Some(s) = &sep {
                    row.push(s.clone());
                }
                row.push(item);
                used += sep_w + w;
            }
            _ => {
                rows.push(vec![item]);
                used = w;
            }
        }
    }
    rows.into_iter().map(Row::Center).collect()
}

/// The panel's rows, top to bottom, for content `width` columns wide.
fn rows(width: usize, icon: bool) -> Vec<Row> {
    let mut out = vec![Row::Pad];
    if icon {
        out.extend((0..ICON_ROWS).map(Row::Icon));
        out.push(Row::Gap);
    }
    out.push(Row::Center(vec![Piece::text(about::NAME, Role::Name)]));
    out.push(Row::Center(vec![Piece::text(about::version_line(VERSION), Role::Body)]));
    out.push(Row::Gap);
    for line in shelbi_app::overlay::error_log::wrap(about::TAGLINE, width) {
        out.push(Row::Center(vec![Piece::text(line, Role::Body)]));
    }
    out.push(Row::Gap);
    out.extend(pack(
        about::LINKS_ROW.into_iter().map(|l| Piece::link(l, Role::Link)).collect(),
        Some(Piece::text(" ", Role::Body)),
        width,
    ));
    out.push(Row::Gap);
    out.push(Row::Center(vec![Piece::link(AboutLink::Acknowledgements, Role::Link)]));
    out.push(Row::Gap);
    out.push(Row::Center(vec![Piece::text(about::copyright(), Role::Muted)]));
    out.extend(pack(
        about::CREDITS_ROW.into_iter().map(|l| Piece::link(l, Role::MutedLink)).collect(),
        Some(Piece::text("·", Role::Muted)),
        width,
    ));
    out.push(Row::Gap);
    out.extend([ButtonPart::Top, ButtonPart::Label, ButtonPart::Bottom].map(Row::Button));
    out.push(Row::Pad);
    out
}

/// Where everything lands for one window size.
struct Layout {
    panel: Rect,
    /// The content's columns (the panel less its side padding).
    content: Rect,
    /// Each drawn row and its screen row.
    rows: Vec<(u16, Row)>,
}

impl Layout {
    /// The button's left column: flush with the content's right edge.
    fn button_x(&self) -> u16 {
        self.content.right().saturating_sub(BUTTON_W).max(self.panel.x)
    }

    /// The target at a screen cell, if any.
    fn hit(&self, col: u16, row: u16) -> Option<Target> {
        for (y, r) in &self.rows {
            match r {
                Row::Center(pieces) if *y == row => {
                    let mut x = self.content.x + self.content.width.saturating_sub(pieces_width(pieces)) / 2;
                    for p in pieces {
                        let w = p.text.width() as u16;
                        if (x..x + w).contains(&col) {
                            return p.target;
                        }
                        x += w;
                    }
                }
                Row::Button(_) if *y == row => {
                    let x = self.button_x();
                    if (x..x + BUTTON_W).contains(&col) {
                        return Some(Target::Close);
                    }
                }
                _ => {}
            }
        }
        None
    }
}

/// Fit the panel to `area`: the icon when there's room for it, then the
/// gaps, then the padding give way; a window too short even then shows the
/// top of the panel.
fn layout(area: Rect) -> Layout {
    let width = WIDTH.min(area.width);
    let pad_x = PAD_X.min(width / 2);
    let content_w = width.saturating_sub(2 * pad_x);
    // The border's two rows sit outside the rows.
    let fits = |rows: &[Row]| rows.len() as u16 + 2 <= area.height;
    let mut planned = rows(content_w as usize, true);
    if !fits(&planned) || content_w < about_icon::SIZE as u16 {
        planned = rows(content_w as usize, false);
    }
    for blank in [Row::Gap, Row::Pad] {
        while !fits(&planned) {
            match planned.iter().rposition(|r| *r == blank) {
                Some(i) => {
                    planned.remove(i);
                }
                None => break,
            }
        }
    }
    let height = (planned.len() as u16 + 2).min(area.height);
    let panel = super::centered_rect(area, width, height);
    let content = Rect {
        x: panel.x + pad_x,
        width: content_w,
        ..panel
    };
    let rows = planned
        .into_iter()
        .enumerate()
        .map(|(i, r)| (panel.y + 1 + i as u16, r))
        .take_while(|(y, _)| *y + 1 < panel.bottom())
        .collect();
    Layout { panel, content, rows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::BLEED_ABOVE;
    use crossterm::event::KeyModifiers;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use shelbi_app::theme::Appearance;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn draw(about: &About, w: u16, h: u16) -> Buffer {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            let area = f.area();
            about.render(f.buffer_mut(), area)
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    fn lines(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn row_of(buf: &Buffer, needle: &str) -> u16 {
        lines(buf)
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} isn't drawn:\n{}", lines(buf).join("\n"))) as u16
    }

    /// The cell where `needle` starts.
    fn cell_of(buf: &Buffer, needle: &str) -> (u16, u16) {
        let y = row_of(buf, needle);
        let line = &lines(buf)[y as usize];
        let at = line.find(needle).unwrap();
        (line[..at].width() as u16, y)
    }

    /// Whether a cell is the icon's: a `▀` in a 24-bit color that isn't
    /// the panel's or the button's (their half rows use `▀` too).
    fn is_icon(cell: &ratatui::buffer::Cell) -> bool {
        let chrome = [palette().search, palette().cyan, palette().selection];
        cell.symbol() == BLEED_BELOW && matches!(cell.fg, Color::Rgb(..)) && !chrome.contains(&cell.fg)
    }

    /// Rows holding icon cells.
    fn icon_rows(buf: &Buffer) -> Vec<u16> {
        (0..buf.area.height)
            .filter(|&y| (0..buf.area.width).any(|x| is_icon(&buf[(x, y)])))
            .collect()
    }

    /// Every line of the desktop's window, in its order.
    fn expect_the_desktop_words(buf: &Buffer) {
        let order = [
            about::NAME.to_string(),
            about::version_line(VERSION),
            about::TAGLINE.to_string(),
            "Website   Documentation   GitHub".to_string(),
            about::copyright(),
            "MIT License · Sponsored by 32pixels".to_string(),
            CLOSE.to_string(),
        ];
        let rows: Vec<u16> = order.iter().map(|s| row_of(buf, s)).collect();
        assert!(rows.windows(2).all(|w| w[0] < w[1]), "in the desktop's order: {rows:?}");
    }

    #[test]
    fn renders_everything_with_the_icon_where_there_is_room() {
        for (w, h) in [(100, 30), (160, 45)] {
            let buf = draw(&About::new(true), w, h);
            expect_the_desktop_words(&buf);
            let icon = icon_rows(&buf);
            assert_eq!(icon.len(), ICON_ROWS, "{w}x{h}: the whole icon");
            assert!(icon.windows(2).all(|r| r[1] == r[0] + 1), "{w}x{h}: one block");
            assert!(icon[ICON_ROWS - 1] < row_of(&buf, about::NAME), "above the name");
            // Centered over the name.
            let (name_x, _) = cell_of(&buf, about::NAME);
            let icon_x = (0..w).find(|&x| is_icon(&buf[(x, icon[3])])).unwrap();
            let icon_mid = icon_x + about_icon::SIZE as u16 / 2;
            let name_mid = name_x + about::NAME.len() as u16 / 2;
            assert!(icon_mid.abs_diff(name_mid) <= 1, "{w}x{h}: {icon_mid} vs {name_mid}");
        }
    }

    #[test]
    fn a_short_window_drops_the_icon_and_keeps_every_line() {
        let buf = draw(&About::new(true), 60, 20);
        expect_the_desktop_words(&buf);
        assert!(icon_rows(&buf).is_empty(), "no partial icon");
        // Shorter still: the gaps go, the words stay.
        let buf = draw(&About::new(true), 60, 14);
        expect_the_desktop_words(&buf);
    }

    #[test]
    fn a_narrow_window_wraps_the_tagline_and_the_links() {
        let buf = draw(&About::new(true), 30, 30);
        let text = lines(&buf).join("\n");
        for word in ["Shelbi", "Website", "Documentation", "GitHub", "MIT License", "Sponsored by 32pixels", "Close"] {
            assert!(text.contains(word), "{word}:\n{text}");
        }
        assert!(text.contains("An Open Source personal"), "{text}");
        for line in lines(&buf) {
            assert!(line.width() <= 30);
        }
    }

    #[test]
    fn a_tiny_window_draws_without_panicking() {
        for (w, h) in [(1, 1), (8, 3), (20, 6), (0, 0)] {
            let about = About::new(true);
            let area = Rect::new(0, 0, w, h);
            let rect = about.rect(area);
            assert!(rect.width <= w && rect.height <= h);
            if w > 0 && h > 0 {
                draw(&about, w, h);
            }
        }
    }

    #[test]
    fn the_panel_has_the_dialog_border_and_a_solid_fill() {
        for (w, h) in [(100, 30), (60, 20)] {
            let buf = draw(&About::new(true), w, h);
            let panel = About::new(true).rect(Rect::new(0, 0, w, h));
            crate::overlay::assert_dialog_border(&buf, panel).unwrap_or_else(|e| panic!("{w}x{h}: {e}"));
            // No half-block cap row is left above or below the content.
            for y in [panel.top(), panel.bottom() - 1] {
                assert!((panel.left()..panel.right()).all(|x| ![BLEED_ABOVE, BLEED_BELOW].contains(&buf[(x, y)].symbol())));
            }
            // Every cell sits on the fill, the border's too, except the
            // icon's and the button's (and a focused link's).
            let close_y = row_of(&buf, CLOSE);
            let icon = icon_rows(&buf);
            for y in panel.top()..panel.bottom() {
                if icon.contains(&y) || (close_y - 1..=close_y + 1).contains(&y) {
                    continue;
                }
                for x in panel.left()..panel.right() {
                    assert_eq!(buf[(x, y)].bg, palette().search, "{w}x{h} ({x},{y})");
                }
            }
            // The content stays inside the border.
            let close = cell_of(&buf, CLOSE);
            assert!(close.0 + CLOSE.len() as u16 + 2 < panel.right() - 1, "{w}x{h}: Close inside");
        }
    }

    #[test]
    fn the_close_button_is_a_half_block_pill_at_the_bottom_right() {
        let buf = draw(&About::new(true), 100, 30);
        let panel = About::new(true).rect(Rect::new(0, 0, 100, 30));
        let (x, y) = cell_of(&buf, CLOSE);
        let left = x - 2;
        assert_eq!(left + BUTTON_W, panel.right() - PAD_X, "flush with the content's right edge");
        // Focused at open: a button's focus fill, its half rows above and below.
        for dx in 0..BUTTON_W {
            assert_eq!(buf[(left + dx, y)].bg, palette().button_focus);
            assert_eq!(buf[(left + dx, y - 1)].symbol(), BLEED_ABOVE);
            assert_eq!(buf[(left + dx, y - 1)].fg, palette().button_focus);
            assert_eq!(buf[(left + dx, y + 1)].symbol(), BLEED_BELOW);
            assert_eq!(buf[(left + dx, y + 1)].fg, palette().button_focus);
        }
        // Unfocused, Settings' Secondary button.
        let mut about = About::new(true);
        about.handle_key(key(KeyCode::Tab));
        let buf = draw(&about, 100, 30);
        assert_eq!(buf[(x, y)].bg, palette().button);
        assert_eq!(buf[(x, y - 1)].fg, palette().button);
    }

    #[test]
    fn tab_walks_the_links_then_close_and_shift_tab_walks_back() {
        let mut about = About::new(true);
        assert_eq!(about.focus(), Target::Close);
        let mut seen = Vec::new();
        for _ in 0..7 {
            about.handle_key(key(KeyCode::Tab));
            seen.push(about.focus());
        }
        use AboutLink::*;
        assert_eq!(
            seen,
            [
                Target::Link(Website),
                Target::Link(Documentation),
                Target::Link(GitHub),
                Target::Link(Acknowledgements),
                Target::Link(License),
                Target::Link(Sponsor),
                Target::Close,
            ]
        );
        about.handle_key(key(KeyCode::BackTab));
        assert_eq!(about.focus(), Target::Link(Sponsor));
        about.handle_key(key(KeyCode::Left));
        assert_eq!(about.focus(), Target::Link(License));
        about.handle_key(key(KeyCode::Right));
        assert_eq!(about.focus(), Target::Link(Sponsor));
    }

    #[test]
    fn enter_or_space_opens_the_focused_link_and_close_closes() {
        let mut about = About::new(true);
        assert_eq!(about.handle_key(key(KeyCode::Enter)), Step::Close);
        let mut opened = Vec::new();
        for _ in 0..6 {
            about.handle_key(key(KeyCode::Tab));
            opened.push(about.handle_key(key(KeyCode::Enter)));
        }
        let open = |url| Step::Open(url);
        assert_eq!(
            opened,
            [
                open(about::WEBSITE_URL),
                open(about::DOCS_URL),
                open(about::GITHUB_URL),
                Step::Acknowledgements,
                open(about::LICENSE_URL),
                open(about::SPONSOR_URL),
            ]
        );
        assert_eq!(about.handle_key(key(KeyCode::Char(' '))), Step::Open(about::SPONSOR_URL));
        assert_eq!(about.handle_key(key(KeyCode::Esc)), Step::Close);
    }

    #[test]
    fn clicks_open_links_and_close_and_misses_do_nothing() {
        let area = Rect::new(0, 0, 100, 30);
        let buf = draw(&About::new(true), 100, 30);
        let click = |about: &mut About, needle: &str| {
            let (x, y) = cell_of(&buf, needle);
            about.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y), area)
        };
        let mut about = About::new(true);
        for (label, url) in [
            ("Website", about::WEBSITE_URL),
            ("Documentation", about::DOCS_URL),
            ("GitHub", about::GITHUB_URL),
            ("MIT License", about::LICENSE_URL),
            ("Sponsored by 32pixels", about::SPONSOR_URL),
        ] {
            assert_eq!(click(&mut about, label), Step::Open(url), "{label}");
        }
        assert_eq!(about.focus(), Target::Link(AboutLink::Sponsor), "a click focuses");
        // Plain words, the panel's padding, and outside it are dead space.
        for needle in [about::NAME, "Version", "agent orchestrator", "©", "·"] {
            assert_eq!(click(&mut about, needle), Step::Stay, "{needle}");
        }
        let miss = mouse(MouseEventKind::Down(MouseButton::Left), 0, 0);
        assert_eq!(about.handle_mouse(miss, area), Step::Stay);
        // The button's half rows are the button too.
        let (x, y) = cell_of(&buf, CLOSE);
        for row in [y - 1, y, y + 1] {
            let down = mouse(MouseEventKind::Down(MouseButton::Left), x, row);
            assert_eq!(About::new(true).handle_mouse(down, area), Step::Close);
        }
    }

    #[test]
    fn the_pointer_underlines_the_link_under_it_and_focus_fills_it() {
        let area = Rect::new(0, 0, 100, 30);
        let mut about = About::new(true);
        let buf = draw(&about, 100, 30);
        let (x, y) = cell_of(&buf, "GitHub");
        about.handle_mouse(mouse(MouseEventKind::Moved, x, y), area);
        let buf = draw(&about, 100, 30);
        assert!(buf[(x, y)].modifier.contains(Modifier::UNDERLINED));
        assert_eq!(buf[(x, y)].fg, palette().cyan);
        let (wx, wy) = cell_of(&buf, "Website");
        assert!(!buf[(wx, wy)].modifier.contains(Modifier::UNDERLINED));

        about.handle_key(key(KeyCode::Tab));
        let buf = draw(&about, 100, 30);
        // The pill covers the label and its padding column each side.
        for dx in 0..("Website".len() as u16 + 2) {
            assert_eq!(buf[(wx - 1 + dx, wy)].bg, palette().selection);
        }
        assert_eq!(buf[(wx, wy)].fg, palette().white);
        assert_eq!(buf[(wx + 8, wy)].bg, palette().search, "the gap after it is the panel's");
    }

    #[test]
    fn a_hovered_link_underlines_its_label_and_not_its_padding() {
        let area = Rect::new(0, 0, 100, 30);
        let labels = ["Website", "Documentation", "GitHub", "Acknowledgements", "MIT License", "Sponsored by 32pixels"];
        for label in labels {
            let mut about = About::new(true);
            let (x, y) = cell_of(&draw(&about, 100, 30), label);
            about.handle_mouse(mouse(MouseEventKind::Moved, x, y), area);
            let buf = draw(&about, 100, 30);
            let underlined = |x: u16| buf[(x, y)].modifier.contains(Modifier::UNDERLINED);
            let w = label.width() as u16;
            assert!((x..x + w).all(underlined), "{label}: every cell of the label");
            assert!(!underlined(x - 1), "{label}: not the cell before it");
            assert!(!underlined(x + w), "{label}: not the cell after it");
            // The padding is still the link's: hovering it underlines the label.
            about.handle_mouse(mouse(MouseEventKind::Moved, x - 1, y), area);
            let buf = draw(&about, 100, 30);
            assert!(buf[(x, y)].modifier.contains(Modifier::UNDERLINED), "{label}: hovered from its padding");
            assert!(!buf[(x - 1, y)].modifier.contains(Modifier::UNDERLINED));
        }
    }

    #[test]
    fn link_colors_follow_the_desktop() {
        // Moving focus off every link: the top row reads in the link color,
        // the credits muted, as the desktop's window draws them.
        let buf = draw(&About::new(true), 100, 30);
        let fg = |needle: &str| {
            let (x, y) = cell_of(&buf, needle);
            buf[(x, y)].fg
        };
        for link in ["Website", "Documentation", "GitHub"] {
            assert_eq!(fg(link), palette().cyan, "{link}");
        }
        for muted in ["MIT License", "Sponsored", "©", "·"] {
            assert_eq!(fg(muted), palette().muted, "{muted}");
        }
        assert_eq!(fg(about::NAME), palette().white);
        assert_eq!(fg("Version"), palette().foreground);
        assert_eq!(fg("An Open Source"), palette().foreground);
    }

    #[test]
    fn without_truecolor_the_icon_uses_the_256_color_palette() {
        let buf = draw(&About::new(false), 100, 30);
        let cells: Vec<_> = (0..30)
            .flat_map(|y| (0..100).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol() == BLEED_BELOW && matches!(buf[(x, y)].fg, Color::Indexed(_)))
            .collect();
        assert!(cells.len() > 100, "the icon is drawn: {}", cells.len());
        assert!(icon_rows(&buf).is_empty(), "no 24-bit color");
    }

    #[test]
    fn renders_readable_in_light() {
        crate::theme::with_appearance(Appearance::Light, || {
            let mut about = About::new(true);
            for (name, steps) in [("about-close", 0), ("about-website", 1), ("about-license", 4)] {
                for _ in 0..steps {
                    about.handle_key(key(KeyCode::Tab));
                }
                for (w, h) in [(100, 30), (60, 20)] {
                    let buf = draw(&about, w, h);
                    crate::theme::assert_light_readable(&format!("{name}-{w}x{h}"), &buf);
                }
            }
        });
    }

    #[test]
    fn the_icon_tables_are_the_desktop_icons_cut_to_the_tile() {
        assert_eq!(about_icon::SIZE % 2, 0, "two pixels to a cell");
        for icon in AppIcon::ALL {
            // The tile fills the frame: its middle is opaque, its rounded
            // corners clear.
            let px = |x: usize, y: usize| pixels(icon)[y * about_icon::SIZE + x];
            let mid = about_icon::SIZE / 2;
            assert!(px(mid, mid)[3] > 250, "{icon:?}");
            assert_eq!(px(0, 0)[3], 0, "{icon:?}");
            // The cobalt tile: blue leads in its corner, beside the stripes.
            let [r, g, b, _] = px(3, 3);
            assert!(b > r && b > g, "{icon:?} blue: {r},{g},{b}");
        }
        assert_ne!(about_icon::COMMAND_GRILLE, about_icon::RETRO);
    }

    /// About draws the icon it was opened with: Command Grille unless told
    /// Retro.
    #[test]
    fn about_draws_the_chosen_icon() {
        let (w, h) = (100, 30);
        let panel = About::new(true).rect(Rect::new(0, 0, w, h));
        let size = about_icon::SIZE as u16;
        let x0 = panel.x + (panel.width - size) / 2;
        // The icon's cells: the first row of the panel that draws `▀`.
        let icon_cells = |about: &About| {
            let buf = draw(about, w, h);
            let y = (panel.y..panel.bottom())
                .find(|&y| (x0..x0 + size).any(|x| buf[(x, y)].symbol() == BLEED_BELOW))
                .expect("the icon is drawn");
            (y..y + ICON_ROWS as u16)
                .flat_map(|y| (x0..x0 + size).map(move |x| (x, y)))
                .map(|(x, y)| (buf[(x, y)].fg, buf[(x, y)].bg))
                .collect::<Vec<_>>()
        };
        assert_eq!(About::new(true).icon(), AppIcon::CommandGrille);
        let grille = icon_cells(&About::new(true));
        assert_eq!(icon_cells(&About::new(true).with_icon(AppIcon::CommandGrille)), grille);
        let retro = icon_cells(&About::new(true).with_icon(AppIcon::Retro));
        assert_ne!(retro, grille);
        // Its middle cell is that table's pixel, drawn in 24-bit color.
        let mid = about_icon::SIZE / 2;
        let at = (ICON_ROWS / 2) * about_icon::SIZE + mid;
        let [r, g, b, _] = about_icon::RETRO[(2 * (ICON_ROWS / 2)) * about_icon::SIZE + mid];
        assert_eq!(retro[at].0, Color::Rgb(r, g, b));
    }
}
