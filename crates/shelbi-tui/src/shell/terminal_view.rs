//! The terminal-view widget: paints a session's client-side emulator
//! ([`shelbi_term::view::TerminalView`]) into a sub-rectangle of the shell and
//! encodes local key / mouse / paste / focus events into the bytes the bound
//! session consumes.
//!
//! This mirrors the single-session `shelbi session attach` client
//! (`shelbi-cli/src/commands/session_attach.rs`): the same emulator, the same
//! cell → ratatui style mapping, the same `viewport::fit` clip/letterbox, and
//! the same input encoding. The difference is that here the view lives in the
//! shell's main area beside a sidebar rather than owning the whole screen, and
//! the color mapping gains a truecolor → 256 downgrade for terminals (or outer
//! multiplexers) that do not carry 24-bit color (plan, "Running inside tmux or
//! Screen"). The two could later share one widget; they are kept separate for
//! now to avoid churn on the in-review attach client.

use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color as VtColor, NamedColor};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton as CtMouseButton,
    MouseEvent as CtMouseEvent, MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color as RatColor, Modifier, Style};

use shelbi_client::SessionEvent;
use shelbi_term::input::{self, Key, Modifiers};
use shelbi_term::selection;
use shelbi_term::view::TerminalView;
use shelbi_term::viewport::{self, Placement};
use shelbi_term::Size;

/// What a mouse event over the pane resolved to.
#[derive(Debug)]
pub enum MouseOutcome {
    /// Bytes to forward to the agent (program mouse reporting).
    Forward(Vec<u8>),
    /// Shelbi handled it locally (scrollback / selection); redraw.
    Handled,
    /// A completed selection whose text should be copied.
    Copy(String),
    /// Nothing to do.
    Ignored,
}

/// Where a left-button gesture is in its lifecycle. A gesture's ownership is
/// decided once, at press time: a plain press is *buffered* (we don't yet know
/// whether it will become a click or a drag), the first motion turns it into a
/// Shelbi selection, and a release with no motion is a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragState {
    /// No left button is down.
    Idle,
    /// Left is down at this viewer cell but has not moved yet — we don't know
    /// whether it will become a click or a drag. The first motion makes it a
    /// Shelbi selection anchored here; a release with no motion is a click.
    /// `forward_click` records what that click does: forward a press+release to
    /// the program (the program is reporting and nothing reclaimed the gesture)
    /// or, when clear, just drop the stale highlight (a bare Shelbi click).
    Pressed { col: u16, row: u16, forward_click: bool },
    /// A Shelbi selection drag is underway.
    Selecting,
    /// The gesture belongs to the program (the Alt escape hatch): every event
    /// is forwarded, press through release.
    Program,
}

/// One session rendered into the main area.
pub struct TerminalPane {
    view: TerminalView,
    /// The session's own size (what the PTY is currently locked to). The pane
    /// clips or letterboxes this into whatever area it is drawn in.
    session_size: Size,
    /// A pending terminal bell to ring on the next frame.
    pub bell: bool,
    /// Set once the session's child has exited.
    pub exited: Option<shelbi_proto::Exited>,
    /// Lifecycle of the current left-button gesture (selection vs. click).
    drag: DragState,
}

impl TerminalPane {
    pub fn new(session_size: Size) -> Self {
        Self {
            view: TerminalView::new(session_size),
            session_size,
            bell: false,
            exited: None,
            drag: DragState::Idle,
        }
    }

    /// The session's current grid size (what the PTY is locked to). Used by
    /// tests to assert the pane reflowed to a requested viewport size.
    #[cfg(test)]
    pub(crate) fn session_size(&self) -> Size {
        self.session_size
    }

    /// True while the view is scrolled up into scrollback (or searching), i.e.
    /// Shelbi — not the agent — owns navigation keys.
    pub fn in_scrollback(&self) -> bool {
        self.view.scroll_offset() > 0 || self.view.search_current().is_some()
    }

    /// Apply one session event. Returns `true` if the screen changed.
    pub fn apply_event(&mut self, ev: SessionEvent) -> bool {
        match ev {
            SessionEvent::Resync { replay, .. } => {
                self.view.feed(&replay);
                true
            }
            SessionEvent::Output { data, .. } => {
                self.view.feed(&data);
                true
            }
            SessionEvent::Resized { cols, rows, .. } | SessionEvent::SizeChanged { cols, rows } => {
                self.session_size = Size::new(cols.max(1), rows.max(1));
                self.view.resize(self.session_size);
                true
            }
            SessionEvent::Title(_) => false,
            SessionEvent::Bell => {
                self.bell = true;
                false
            }
            SessionEvent::Exited(e) => {
                self.exited = Some(e);
                true
            }
        }
    }

    /// Derive the key-encoding modes from the emulator's current terminal mode.
    pub fn key_encoding(&self) -> input::KeyEncoding {
        let mode = self.view.emulator().mode();
        input::KeyEncoding {
            kitty: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
            application_cursor_keys: mode.contains(TermMode::APP_CURSOR),
            newline_mode: mode.contains(TermMode::LINE_FEED_NEW_LINE),
        }
    }

    /// Encode a key press into PTY bytes, snapping the view back to the live
    /// bottom first. Returns `None` for keys outside the emulator's vocabulary.
    pub fn encode_key(&mut self, k: &KeyEvent) -> Option<Vec<u8>> {
        let (key, mods) = map_key(k)?;
        let enc = self.key_encoding();
        let bytes = input::encode_key(key, mods, enc);
        if bytes.is_empty() {
            return None;
        }
        self.view.on_user_input();
        // A key sent to the session dismisses any lingering selection highlight
        // (one of the three things that end it; the others are the next click
        // and new output). Copy chords never reach here — the shell consumes
        // them before forwarding (see `is_copy_key`).
        self.view.clear_selection();
        Some(bytes)
    }

    /// The current non-empty selection as text, for an explicit copy (Cmd+C /
    /// Ctrl+Shift+C). `None` when there is no selection, or it is empty (a bare
    /// click), so the copy chord is a harmless no-op rather than clobbering the
    /// clipboard with an empty string.
    pub fn selection_copy(&self) -> Option<String> {
        if self.view.selection().is_none_or(|s| s.is_empty()) {
            return None;
        }
        self.view.selection_text().filter(|t| !t.is_empty())
    }

    pub fn encode_focus(&self, focused: bool) -> Option<Vec<u8>> {
        input::encode_focus(focused, self.view.emulator().focus_reporting())
    }

    /// Scroll Shelbi's scrollback (a no-op on the alternate screen, enforced by
    /// `TerminalView`).
    pub fn scroll_up(&mut self, lines: usize) {
        self.view.scroll_up(lines);
    }
    pub fn scroll_down(&mut self, lines: usize) {
        self.view.scroll_down(lines);
    }
    pub fn scroll_to_bottom(&mut self) {
        self.view.scroll_to_bottom();
    }

    /// Start (or refine) a literal search across scrollback. Returns the match
    /// count.
    pub fn search(&mut self, query: &str) -> usize {
        self.view.start_search(query, true)
    }
    pub fn search_next(&mut self) {
        self.view.search_next();
    }
    pub fn search_prev(&mut self) {
        self.view.search_prev();
    }
    pub fn clear_search(&mut self) {
        self.view.clear_search();
    }

    /// Handle a mouse event whose coordinates are already pane-relative
    /// (0-based, column/row inside the draw area). `viewer` is the size of that
    /// draw area; `reporting`/`sgr`/`shift` decide ownership. The session's grid
    /// (sized to the most-recently-active client, which may not be us) is
    /// letterboxed or clipped into `viewer`, and that same placement translates
    /// the click back to a session cell — so a click in the letterbox margin or
    /// outside a clipped window maps to nothing.
    pub fn on_mouse(
        &mut self,
        m: &CtMouseEvent,
        pane_col: u16,
        pane_row: u16,
        viewer: Size,
    ) -> MouseOutcome {
        let mode = self.view.emulator().mode();
        let reporting = mode.intersects(TermMode::MOUSE_MODE);
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        let placement = viewport::fit(self.session_size, viewer);
        match m.kind {
            // The left button is the selection/click gesture — a small state
            // machine (see `DragState`) decides per gesture between a Shelbi
            // selection and a forwarded click.
            MouseEventKind::Down(CtMouseButton::Left)
            | MouseEventKind::Drag(CtMouseButton::Left)
            | MouseEventKind::Up(CtMouseButton::Left) => {
                self.left_mouse(m, pane_col, pane_row, &placement)
            }
            // Wheel and the other buttons keep the simple ownership policy: the
            // program gets them when it is reporting (unless Shift reclaims
            // them); otherwise the wheel scrolls Shelbi's scrollback.
            _ => match input::mouse_owner(reporting, shift) {
                input::MouseOwner::Shelbi => match m.kind {
                    MouseEventKind::ScrollUp => {
                        self.view.scroll_up(3);
                        MouseOutcome::Handled
                    }
                    MouseEventKind::ScrollDown => {
                        self.view.scroll_down(3);
                        MouseOutcome::Handled
                    }
                    _ => MouseOutcome::Ignored,
                },
                input::MouseOwner::Program => self.forward_mouse(m, pane_col, pane_row, &placement),
            },
        }
    }

    /// Encode a mouse event for the program, if a session cell lies under it.
    fn forward_mouse(
        &self,
        m: &CtMouseEvent,
        col: u16,
        row: u16,
        placement: &Placement,
    ) -> MouseOutcome {
        let Some(ev) = map_mouse(m, col, row) else {
            return MouseOutcome::Ignored;
        };
        match input::encode_mouse(&ev, self.mouse_modes(), placement) {
            Some(bytes) => MouseOutcome::Forward(bytes),
            None => MouseOutcome::Ignored,
        }
    }

    fn mouse_modes(&self) -> input::MouseModes {
        let mode = self.view.emulator().mode();
        input::MouseModes {
            reporting: mode.intersects(TermMode::MOUSE_MODE),
            sgr: mode.contains(TermMode::SGR_MOUSE),
        }
    }

    /// Left-button handling: a plain drag selects (even when the program has
    /// mouse reporting on), a plain click reaches the program, Shift forces a
    /// selection, and Alt forwards the whole gesture to the program (the escape
    /// hatch for programs that need drags). A finished selection copies.
    fn left_mouse(
        &mut self,
        m: &CtMouseEvent,
        col: u16,
        row: u16,
        placement: &Placement,
    ) -> MouseOutcome {
        let reporting = self.view.emulator().mode().intersects(TermMode::MOUSE_MODE);
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        let alt = m.modifiers.contains(KeyModifiers::ALT);
        match m.kind {
            MouseEventKind::Down(CtMouseButton::Left) => {
                // A new press ends any lingering highlight (the "next click"
                // rule) and we defer the selection-vs-click decision to the
                // first motion. Alt+reporting is the one exception: it hands the
                // whole gesture to the program up front.
                self.view.clear_selection();
                if reporting && !shift && alt {
                    self.drag = DragState::Program;
                    return self.forward_mouse(m, col, row, placement);
                }
                // A click (no drag) forwards to the program only when the
                // program is reporting and did not reclaim the gesture via
                // Shift; otherwise a bare click is Shelbi's and does nothing.
                let forward_click = reporting && !shift;
                self.drag = DragState::Pressed { col, row, forward_click };
                MouseOutcome::Handled
            }
            MouseEventKind::Drag(CtMouseButton::Left) => match self.drag {
                DragState::Program => self.forward_mouse(m, col, row, placement),
                DragState::Pressed { col: c0, row: r0, .. } => {
                    // First motion: this gesture is a drag, so it is a Shelbi
                    // selection anchored at the press point.
                    if let Some(p) = self.view.viewer_point_to_grid(placement, c0, r0) {
                        self.view.begin_selection(p);
                    }
                    if let Some(p) = self.view.viewer_point_to_grid(placement, col, row) {
                        self.view.update_selection(p);
                    }
                    self.drag = DragState::Selecting;
                    MouseOutcome::Handled
                }
                DragState::Selecting => {
                    if let Some(p) = self.view.viewer_point_to_grid(placement, col, row) {
                        self.view.update_selection(p);
                    }
                    MouseOutcome::Handled
                }
                DragState::Idle => MouseOutcome::Handled,
            },
            MouseEventKind::Up(CtMouseButton::Left) => {
                match std::mem::replace(&mut self.drag, DragState::Idle) {
                    DragState::Program => self.forward_mouse(m, col, row, placement),
                    DragState::Selecting => self.finish_selection(),
                    DragState::Pressed { col: c0, row: r0, forward_click } => {
                        if forward_click {
                            // A plain click while the program is reporting: now
                            // that we know it never became a drag, forward the
                            // buffered press and this release as a clean click.
                            self.forward_click(c0, r0, col, row, placement)
                        } else {
                            // A bare Shelbi click: the old highlight was already
                            // dropped on the press; nothing more to do.
                            MouseOutcome::Handled
                        }
                    }
                    DragState::Idle => MouseOutcome::Ignored,
                }
            }
            _ => MouseOutcome::Ignored,
        }
    }

    /// Resolve a finished selection drag: copy it when it actually spans text,
    /// otherwise drop the stray highlight of a drag that never left its anchor.
    fn finish_selection(&mut self) -> MouseOutcome {
        let dragged = self.view.selection().is_some_and(|s| !s.is_empty());
        match self.view.selection_text() {
            Some(text) if dragged && !text.is_empty() => MouseOutcome::Copy(text),
            _ => {
                self.view.clear_selection();
                MouseOutcome::Handled
            }
        }
    }

    /// Forward a buffered click (press at `c0,r0`, release at `cr,rr`) to the
    /// program as a press immediately followed by a release.
    fn forward_click(
        &self,
        c0: u16,
        r0: u16,
        cr: u16,
        rr: u16,
        placement: &Placement,
    ) -> MouseOutcome {
        let modes = self.mouse_modes();
        let ev = |action, col, row| input::MouseEvent {
            action,
            button: input::MouseButton::Left,
            col,
            row,
            mods: Modifiers::default(),
        };
        let mut bytes = Vec::new();
        if let Some(down) = input::encode_mouse(&ev(input::MouseAction::Press, c0, r0), modes, placement) {
            bytes.extend_from_slice(&down);
        }
        if let Some(up) = input::encode_mouse(&ev(input::MouseAction::Release, cr, rr), modes, placement) {
            bytes.extend_from_slice(&up);
        }
        if bytes.is_empty() {
            MouseOutcome::Ignored
        } else {
            MouseOutcome::Forward(bytes)
        }
    }

    /// Paint the pane into `area`, clipping or letterboxing the session grid.
    /// Returns the viewer cursor position when it is visible.
    pub fn render(&self, buf: &mut Buffer, area: Rect, truecolor: bool) -> Option<(u16, u16)> {
        render_grid(buf, area, &self.view, self.session_size, truecolor)
    }
}

/// Paint the session's visible grid into `buf` over `area`.
fn render_grid(
    buf: &mut Buffer,
    area: Rect,
    view: &TerminalView,
    session_size: Size,
    truecolor: bool,
) -> Option<(u16, u16)> {
    let viewer = Size::new(area.width, area.height);
    let placement = viewport::fit(session_size, viewer);
    // Blank the whole area first: letterbox margins and any stale cells.
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
            }
        }
    }

    let emu = view.emulator();
    let grid = emu.grid();
    let offset = emu.display_offset() as i32;
    let rrows = placement.rows;
    let rcols = placement.cols;
    // The selection's grid-line coordinates line up with `display_iter`'s point
    // lines (both are screen-relative: line 0 is the top of the visible grid),
    // so a cell is highlighted exactly when the selection contains its point.
    let selection = view.selection();

    for indexed in grid.display_iter() {
        let srow = indexed.point.line.0 + offset;
        if srow < 0 {
            continue;
        }
        let (srow, scol) = (srow as u32, indexed.point.column.0 as u32);
        if srow >= rrows.len as u32 || scol >= rcols.len as u32 {
            continue;
        }
        let vx = area.left() + rcols.pad_before + scol as u16;
        let vy = area.top() + rrows.pad_before + srow as u16;
        let Some(out) = buf.cell_mut((vx, vy)) else {
            continue;
        };
        let cell = indexed.cell;
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            out.set_symbol("");
            continue;
        }
        out.set_char(if cell.c == '\0' { ' ' } else { cell.c });
        let mut style = cell_style(cell, truecolor);
        if selection
            .is_some_and(|s| s.contains(indexed.point.line.0, indexed.point.column.0 as u16))
        {
            // Reverse video is the toolkit- and theme-independent highlight (the
            // same device the detach hint uses), so the selection reads on any
            // palette without choosing a background color.
            style = style.add_modifier(Modifier::REVERSED);
        }
        out.set_style(style);
    }

    if emu.mode().contains(TermMode::SHOW_CURSOR) {
        let cp = grid.cursor.point;
        let crow = cp.line.0 + offset;
        let ccol = cp.column.0 as i32;
        if crow >= 0
            && (crow as u32) < rrows.len as u32
            && ccol >= 0
            && (ccol as u32) < rcols.len as u32
        {
            let vx = area.left() + rcols.pad_before + ccol as u16;
            let vy = area.top() + rrows.pad_before + crow as u16;
            return Some((vx, vy));
        }
    }
    None
}

/// Translate an alacritty cell into a ratatui style. When `truecolor` is false,
/// explicit RGB is quantized to the xterm-256 palette so the pane renders on a
/// terminal (or through an outer multiplexer) that cannot carry 24-bit color.
fn cell_style(cell: &Cell, truecolor: bool) -> Style {
    let (mut fg, mut bg) = (conv_color(cell.fg, truecolor), conv_color(cell.bg, truecolor));
    let f = cell.flags;
    if f.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    let mut style = Style::default().fg(fg).bg(bg);
    if f.contains(Flags::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if f.contains(Flags::DIM) {
        style = style.add_modifier(Modifier::DIM);
    }
    if f.contains(Flags::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if f.intersects(
        Flags::UNDERLINE
            | Flags::DOUBLE_UNDERLINE
            | Flags::UNDERCURL
            | Flags::DOTTED_UNDERLINE
            | Flags::DASHED_UNDERLINE,
    ) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if f.contains(Flags::HIDDEN) {
        style = style.add_modifier(Modifier::HIDDEN);
    }
    if f.contains(Flags::STRIKEOUT) {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    style
}

fn conv_color(c: VtColor, truecolor: bool) -> RatColor {
    match c {
        VtColor::Named(n) => named_to_rat(n),
        VtColor::Indexed(i) => RatColor::Indexed(i),
        VtColor::Spec(rgb) => {
            if truecolor {
                RatColor::Rgb(rgb.r, rgb.g, rgb.b)
            } else {
                RatColor::Indexed(rgb_to_xterm256(rgb.r, rgb.g, rgb.b))
            }
        }
    }
}

/// Quantize a 24-bit color to the closest xterm-256 palette index, using the
/// standard 6×6×6 color cube plus the 24-step grayscale ramp and keeping
/// whichever is nearer.
pub(crate) fn rgb_to_xterm256(r: u8, g: u8, b: u8) -> u8 {
    // 6x6x6 cube: component levels are 0,95,135,175,215,255.
    fn level(v: u8) -> (u8, u8) {
        const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let mut best = 0usize;
        let mut best_d = u16::MAX;
        for (i, &s) in STEPS.iter().enumerate() {
            let d = (s as i16 - v as i16).unsigned_abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        (best as u8, STEPS[best])
    }
    let (ri, rv) = level(r);
    let (gi, gv) = level(g);
    let (bi, bv) = level(b);
    let cube_idx = 16 + 36 * ri + 6 * gi + bi;
    let cube_dist = dist2(r, g, b, rv, gv, bv);

    // Grayscale ramp: indices 232..=255 are gray levels 8,18,...,238.
    let gray = ((r as u16 + g as u16 + b as u16) / 3) as u8;
    let gi = if gray < 8 {
        0
    } else {
        (((gray as i16 - 8) + 5) / 10).clamp(0, 23) as u8
    };
    let gv = 8 + gi * 10;
    let gray_idx = 232 + gi;
    let gray_dist = dist2(r, g, b, gv, gv, gv);

    if gray_dist < cube_dist {
        gray_idx
    } else {
        cube_idx
    }
}

fn dist2(r: u8, g: u8, b: u8, r2: u8, g2: u8, b2: u8) -> u32 {
    let d = |a: u8, b: u8| {
        let x = a as i32 - b as i32;
        (x * x) as u32
    };
    d(r, r2) + d(g, g2) + d(b, b2)
}

fn named_to_rat(n: NamedColor) -> RatColor {
    use NamedColor::*;
    match n {
        Black | DimBlack => RatColor::Black,
        Red | DimRed => RatColor::Red,
        Green | DimGreen => RatColor::Green,
        Yellow | DimYellow => RatColor::Yellow,
        Blue | DimBlue => RatColor::Blue,
        Magenta | DimMagenta => RatColor::Magenta,
        Cyan | DimCyan => RatColor::Cyan,
        White | DimWhite => RatColor::Gray,
        BrightBlack => RatColor::DarkGray,
        BrightRed => RatColor::LightRed,
        BrightGreen => RatColor::LightGreen,
        BrightYellow => RatColor::LightYellow,
        BrightBlue => RatColor::LightBlue,
        BrightMagenta => RatColor::LightMagenta,
        BrightCyan => RatColor::LightCyan,
        BrightWhite => RatColor::White,
        Foreground | Background | Cursor | BrightForeground | DimForeground => RatColor::Reset,
    }
}

/// Map a crossterm key event to shelbi-term's neutral key + modifiers. Returns
/// `None` for keys the encoder has no representation for.
pub(crate) fn map_key(k: &KeyEvent) -> Option<(Key, Modifiers)> {
    let mut mods = Modifiers {
        shift: k.modifiers.contains(KeyModifiers::SHIFT),
        alt: k.modifiers.contains(KeyModifiers::ALT),
        ctrl: k.modifiers.contains(KeyModifiers::CONTROL),
        sup: k.modifiers.contains(KeyModifiers::SUPER),
    };
    let key = match k.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => {
            mods.shift = true;
            Key::Tab
        }
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Esc => Key::Escape,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::F(n) => Key::Function(n),
        _ => return None,
    };
    // A plain typed character already encodes its shift; a lone Shift modifier
    // would make the kitty encoder emit a modified key.
    if matches!(key, Key::Char(_)) && mods.shift && !mods.ctrl && !mods.alt && !mods.sup {
        mods.shift = false;
    }
    Some((key, mods))
}

fn map_mouse(m: &CtMouseEvent, col: u16, row: u16) -> Option<input::MouseEvent> {
    let mods = Modifiers {
        shift: m.modifiers.contains(KeyModifiers::SHIFT),
        alt: m.modifiers.contains(KeyModifiers::ALT),
        ctrl: m.modifiers.contains(KeyModifiers::CONTROL),
        sup: m.modifiers.contains(KeyModifiers::SUPER),
    };
    let btn = |b: CtMouseButton| match b {
        CtMouseButton::Left => input::MouseButton::Left,
        CtMouseButton::Middle => input::MouseButton::Middle,
        CtMouseButton::Right => input::MouseButton::Right,
    };
    let (action, button) = match m.kind {
        MouseEventKind::Down(b) => (input::MouseAction::Press, btn(b)),
        MouseEventKind::Up(b) => (input::MouseAction::Release, btn(b)),
        MouseEventKind::Drag(b) => (input::MouseAction::Drag, btn(b)),
        MouseEventKind::ScrollUp => (input::MouseAction::Press, input::MouseButton::WheelUp),
        MouseEventKind::ScrollDown => (input::MouseAction::Press, input::MouseButton::WheelDown),
        _ => return None,
    };
    Some(input::MouseEvent {
        action,
        button,
        col,
        row,
        mods,
    })
}

/// Whether a crossterm key event is Ctrl+Space, the one key the shell reserves
/// from the agent (it moves focus to the sidebar until overlays land).
pub(crate) fn is_focus_key(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(k.code, KeyCode::Char(' ') | KeyCode::Char('@'))
}

/// Whether this key event should be acted on (a press/repeat, not a release).
pub(crate) fn is_actionable(k: &KeyEvent) -> bool {
    !matches!(k.kind, KeyEventKind::Release)
}

/// Whether `k` is a "copy the selection" chord: Cmd+C (macOS, when the terminal
/// passes `super+c` through rather than handling it itself) or the portable
/// Ctrl+Shift+C. A plain Ctrl+C (no Shift) is deliberately *not* matched, so an
/// interrupt still reaches the agent. The shell consumes a matched chord
/// whether or not a selection exists, so it never forwards a stray `c` /
/// Ctrl+C to the session.
pub(crate) fn is_copy_key(k: &KeyEvent) -> bool {
    if !matches!(k.code, KeyCode::Char('c') | KeyCode::Char('C')) {
        return false;
    }
    let m = k.modifiers;
    m.contains(KeyModifiers::SUPER)
        || (m.contains(KeyModifiers::CONTROL) && m.contains(KeyModifiers::SHIFT))
}

/// Build the OSC 52 copy sequence for `text`.
pub(crate) fn osc52(text: &str) -> Vec<u8> {
    selection::osc52_copy(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::Rgb;
    use crossterm::event::KeyModifiers as KM;

    #[test]
    fn truecolor_rgb_passes_through_but_quantizes_when_off() {
        let cell = Cell {
            c: 'x',
            fg: VtColor::Spec(Rgb { r: 10, g: 200, b: 30 }),
            bg: VtColor::Named(NamedColor::Background),
            flags: Flags::empty(),
            extra: None,
        };
        // Truecolor on: RGB is pinned exactly.
        let s = cell_style(&cell, true);
        assert_eq!(s.fg, Some(RatColor::Rgb(10, 200, 30)));
        // Truecolor off: downgraded to an xterm-256 index.
        let s = cell_style(&cell, false);
        match s.fg {
            Some(RatColor::Indexed(_)) => {}
            other => panic!("expected a 256-color index, got {other:?}"),
        }
    }

    #[test]
    fn rgb_to_256_hits_known_anchors() {
        assert_eq!(rgb_to_xterm256(0, 0, 0), 16, "pure black → cube origin");
        assert_eq!(rgb_to_xterm256(255, 255, 255), 231, "pure white → cube corner");
        assert_eq!(rgb_to_xterm256(255, 0, 0), 196, "pure red → cube red");
        // A mid gray prefers the grayscale ramp (232..=255), not the cube.
        let g = rgb_to_xterm256(128, 128, 128);
        assert!((232..=255).contains(&g), "mid gray uses the gray ramp, got {g}");
    }

    #[test]
    fn inverse_swaps_fg_and_bg() {
        let cell = Cell {
            c: 'x',
            fg: VtColor::Named(NamedColor::Red),
            bg: VtColor::Named(NamedColor::Blue),
            flags: Flags::INVERSE,
            extra: None,
        };
        let s = cell_style(&cell, true);
        assert_eq!(s.fg, Some(RatColor::Blue));
        assert_eq!(s.bg, Some(RatColor::Red));
    }

    #[test]
    fn ctrl_space_is_the_reserved_focus_key() {
        assert!(is_focus_key(&KeyEvent::new(KeyCode::Char(' '), KM::CONTROL)));
        // Many terminals deliver Ctrl+Space as Ctrl+@ (NUL).
        assert!(is_focus_key(&KeyEvent::new(KeyCode::Char('@'), KM::CONTROL)));
        // A plain space, or Ctrl+other, is not the focus key — it reaches the agent.
        assert!(!is_focus_key(&KeyEvent::new(KeyCode::Char(' '), KM::NONE)));
        assert!(!is_focus_key(&KeyEvent::new(KeyCode::Char('a'), KM::CONTROL)));
    }

    #[test]
    fn shift_enter_encodes_through_kitty_when_the_session_requests_it() {
        // A fresh pane: feed the sequence that turns on the kitty keyboard
        // protocol (CSI > 1 u pushes a flags stack with DISAMBIGUATE set).
        let mut pane = TerminalPane::new(Size::new(20, 5));
        pane.view.feed(b"\x1b[>1u");
        let bytes = pane
            .encode_key(&KeyEvent::new(KeyCode::Enter, KM::SHIFT))
            .expect("Shift+Enter encodes to something");
        assert_eq!(bytes, b"\x1b[13;2u", "Shift+Enter rides the kitty CSU form");
    }

    #[test]
    fn plain_enter_is_a_carriage_return_without_kitty() {
        let mut pane = TerminalPane::new(Size::new(20, 5));
        let bytes = pane
            .encode_key(&KeyEvent::new(KeyCode::Enter, KM::NONE))
            .unwrap();
        assert_eq!(bytes, b"\r");
    }

    #[test]
    fn wheel_scrolls_scrollback_and_marks_handled() {
        let mut pane = TerminalPane::new(Size::new(10, 2));
        // Two screens of lines so there is history to scroll into.
        for i in 0..20 {
            pane.view.feed(format!("line{i}\r\n").as_bytes());
        }
        assert!(!pane.in_scrollback());
        let ev = CtMouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KM::NONE,
        };
        match pane.on_mouse(&ev, 0, 0, Size::new(10, 2)) {
            MouseOutcome::Handled => {}
            _ => panic!("wheel without program reporting is Shelbi's scrollback"),
        }
        assert!(pane.in_scrollback(), "scrolled up into history");
    }

    /// Enable SGR mouse reporting in the pane's emulator (what a program such as
    /// an editor does when it wants the mouse).
    fn enable_sgr_mouse(pane: &mut TerminalPane) {
        // DECSET 1000 (report button events) + 1006 (SGR extended coordinates).
        pane.view.feed(b"\x1b[?1000h\x1b[?1006h");
    }

    fn mouse(kind: MouseEventKind, col: u16, row: u16, mods: KM) -> CtMouseEvent {
        CtMouseEvent { kind, column: col, row, modifiers: mods }
    }

    #[test]
    fn program_mouse_click_translates_through_a_letterbox() {
        // Session 80x24 shown in a 100x30 viewer: 10-col / 3-row letterbox
        // margins. A program-owned click at viewer cell (15, 5) maps to session
        // cell (5, 2) → SGR 1-based (6, 3). The press is buffered (we don't yet
        // know if it will become a drag); the release with no motion forwards
        // the click as a press immediately followed by a release.
        let mut pane = TerminalPane::new(Size::new(80, 24));
        enable_sgr_mouse(&mut pane);
        let down = mouse(MouseEventKind::Down(CtMouseButton::Left), 15, 5, KM::NONE);
        assert!(
            matches!(pane.on_mouse(&down, 15, 5, Size::new(100, 30)), MouseOutcome::Handled),
            "the press is buffered until we know click-vs-drag"
        );
        let up = mouse(MouseEventKind::Up(CtMouseButton::Left), 15, 5, KM::NONE);
        match pane.on_mouse(&up, 15, 5, Size::new(100, 30)) {
            MouseOutcome::Forward(bytes) => assert_eq!(bytes, b"\x1b[<0;6;3M\x1b[<0;6;3m".to_vec()),
            other => panic!("a program click forwards press+release on release, got {other:?}"),
        }
        // A click in the letterbox margin covers no session cell → nothing is
        // forwarded.
        let margin_down = mouse(MouseEventKind::Down(CtMouseButton::Left), 2, 5, KM::NONE);
        pane.on_mouse(&margin_down, 2, 5, Size::new(100, 30));
        let margin_up = mouse(MouseEventKind::Up(CtMouseButton::Left), 2, 5, KM::NONE);
        assert!(
            matches!(pane.on_mouse(&margin_up, 2, 5, Size::new(100, 30)), MouseOutcome::Ignored),
            "a click in the letterbox margin maps to no session cell"
        );
    }

    #[test]
    fn program_mouse_click_translates_through_a_clip() {
        // Session 120x40 clipped into an 80x24 viewer (anchored top-left). The
        // bottom-right visible cell (79, 23) maps straight through; the click
        // forwards on release.
        let mut pane = TerminalPane::new(Size::new(120, 40));
        enable_sgr_mouse(&mut pane);
        let down = mouse(MouseEventKind::Down(CtMouseButton::Left), 79, 23, KM::NONE);
        assert!(matches!(
            pane.on_mouse(&down, 79, 23, Size::new(80, 24)),
            MouseOutcome::Handled
        ));
        let up = mouse(MouseEventKind::Up(CtMouseButton::Left), 79, 23, KM::NONE);
        match pane.on_mouse(&up, 79, 23, Size::new(80, 24)) {
            MouseOutcome::Forward(bytes) => {
                assert_eq!(bytes, b"\x1b[<0;80;24M\x1b[<0;80;24m".to_vec())
            }
            other => panic!("a clipped-window click should forward, got {other:?}"),
        }
    }

    #[test]
    fn plain_drag_selects_even_when_the_program_wants_the_mouse() {
        // The headline behavior: with the program reporting the mouse, a plain
        // (no-modifier) left drag is Shelbi's selection, never forwarded, and
        // the release copies.
        let mut pane = TerminalPane::new(Size::new(20, 3));
        enable_sgr_mouse(&mut pane);
        pane.view.feed(b"hello world");
        let down = mouse(MouseEventKind::Down(CtMouseButton::Left), 0, 0, KM::NONE);
        assert!(matches!(
            pane.on_mouse(&down, 0, 0, Size::new(20, 3)),
            MouseOutcome::Handled
        ));
        let drag = mouse(MouseEventKind::Drag(CtMouseButton::Left), 4, 0, KM::NONE);
        assert!(
            matches!(pane.on_mouse(&drag, 4, 0, Size::new(20, 3)), MouseOutcome::Handled),
            "a drag selects locally, never forwarded, even with program reporting on"
        );
        let up = mouse(MouseEventKind::Up(CtMouseButton::Left), 4, 0, KM::NONE);
        match pane.on_mouse(&up, 4, 0, Size::new(20, 3)) {
            MouseOutcome::Copy(text) => assert_eq!(text, "hello"),
            other => panic!("a finished plain drag copies the selection, got {other:?}"),
        }
    }

    #[test]
    fn alt_drag_forwards_to_the_program() {
        // The escape hatch: Alt+drag hands the whole gesture to a reporting
        // program (press, motion, release all forwarded) and selects nothing.
        let mut pane = TerminalPane::new(Size::new(20, 3));
        enable_sgr_mouse(&mut pane);
        pane.view.feed(b"hello world");
        let down = mouse(MouseEventKind::Down(CtMouseButton::Left), 0, 0, KM::ALT);
        // Press: SGR button 0 + Alt bit (8) at 1-based (1,1).
        match pane.on_mouse(&down, 0, 0, Size::new(20, 3)) {
            MouseOutcome::Forward(bytes) => assert_eq!(bytes, b"\x1b[<8;1;1M".to_vec()),
            other => panic!("Alt+press forwards to the program, got {other:?}"),
        }
        let drag = mouse(MouseEventKind::Drag(CtMouseButton::Left), 4, 0, KM::ALT);
        match pane.on_mouse(&drag, 4, 0, Size::new(20, 3)) {
            // motion bit (32) + button 0 + Alt (8) = 40, at 1-based (5,1).
            MouseOutcome::Forward(bytes) => assert_eq!(bytes, b"\x1b[<40;5;1M".to_vec()),
            other => panic!("Alt+drag forwards to the program, got {other:?}"),
        }
        let up = mouse(MouseEventKind::Up(CtMouseButton::Left), 4, 0, KM::ALT);
        assert!(
            matches!(pane.on_mouse(&up, 4, 0, Size::new(20, 3)), MouseOutcome::Forward(_)),
            "Alt+release forwards too"
        );
        assert!(
            pane.selection_copy().is_none(),
            "Alt+drag never builds a Shelbi selection"
        );
    }

    #[test]
    fn selection_persists_until_a_key_is_sent() {
        // After a drag-select the highlight stays (selection_copy still yields
        // the text) through a copy chord, and a key sent to the session clears
        // it.
        let mut pane = TerminalPane::new(Size::new(20, 3));
        pane.view.feed(b"hello world");
        let down = mouse(MouseEventKind::Down(CtMouseButton::Left), 0, 0, KM::NONE);
        pane.on_mouse(&down, 0, 0, Size::new(20, 3));
        let drag = mouse(MouseEventKind::Drag(CtMouseButton::Left), 4, 0, KM::NONE);
        pane.on_mouse(&drag, 4, 0, Size::new(20, 3));
        let up = mouse(MouseEventKind::Up(CtMouseButton::Left), 4, 0, KM::NONE);
        assert!(matches!(pane.on_mouse(&up, 4, 0, Size::new(20, 3)), MouseOutcome::Copy(_)));
        // Highlight (and copyable text) survive the release.
        assert_eq!(pane.selection_copy().as_deref(), Some("hello"));
        // A key sent to the session dismisses it.
        pane.encode_key(&KeyEvent::new(KeyCode::Char('x'), KM::NONE));
        assert!(pane.selection_copy().is_none(), "a session keypress clears the selection");
    }

    #[test]
    fn a_new_click_clears_the_previous_highlight() {
        let mut pane = TerminalPane::new(Size::new(20, 3));
        pane.view.feed(b"hello world");
        // Select "hello".
        pane.on_mouse(&mouse(MouseEventKind::Down(CtMouseButton::Left), 0, 0, KM::NONE), 0, 0, Size::new(20, 3));
        pane.on_mouse(&mouse(MouseEventKind::Drag(CtMouseButton::Left), 4, 0, KM::NONE), 4, 0, Size::new(20, 3));
        pane.on_mouse(&mouse(MouseEventKind::Up(CtMouseButton::Left), 4, 0, KM::NONE), 4, 0, Size::new(20, 3));
        assert!(pane.selection_copy().is_some());
        // A fresh press elsewhere clears the old highlight.
        pane.on_mouse(&mouse(MouseEventKind::Down(CtMouseButton::Left), 8, 0, KM::NONE), 8, 0, Size::new(20, 3));
        assert!(pane.selection_copy().is_none(), "a new click clears the highlight");
    }

    #[test]
    fn selected_cells_render_reversed() {
        let mut pane = TerminalPane::new(Size::new(20, 1));
        pane.view.feed(b"hello world");
        // Select "hello" (cols 0..=4).
        pane.on_mouse(&mouse(MouseEventKind::Down(CtMouseButton::Left), 0, 0, KM::NONE), 0, 0, Size::new(20, 1));
        pane.on_mouse(&mouse(MouseEventKind::Drag(CtMouseButton::Left), 4, 0, KM::NONE), 4, 0, Size::new(20, 1));
        assert!(matches!(
            pane.on_mouse(&mouse(MouseEventKind::Up(CtMouseButton::Left), 4, 0, KM::NONE), 4, 0, Size::new(20, 1)),
            MouseOutcome::Copy(_)
        ));
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        pane.render(&mut buf, area, true);
        let reversed = |x: u16| buf[(x, 0)].modifier.contains(Modifier::REVERSED);
        for x in 0..=4 {
            assert!(reversed(x), "selected cell {x} should be reverse-video");
        }
        assert!(!reversed(5), "the space after the selection is not highlighted");
        assert!(!reversed(6), "'w' past the selection is not highlighted");
    }

    #[test]
    fn copy_key_recognizes_cmd_c_and_ctrl_shift_c_only() {
        // Cmd+C (SUPER) and Ctrl+Shift+C are copy chords.
        assert!(is_copy_key(&KeyEvent::new(KeyCode::Char('c'), KM::SUPER)));
        assert!(is_copy_key(&KeyEvent::new(KeyCode::Char('c'), KM::CONTROL | KM::SHIFT)));
        assert!(is_copy_key(&KeyEvent::new(KeyCode::Char('C'), KM::CONTROL | KM::SHIFT)));
        // A bare Ctrl+C (interrupt) is NOT — it must still reach the agent.
        assert!(!is_copy_key(&KeyEvent::new(KeyCode::Char('c'), KM::CONTROL)));
        // A plain typed `c` is not a copy chord.
        assert!(!is_copy_key(&KeyEvent::new(KeyCode::Char('c'), KM::NONE)));
    }

    #[test]
    fn shift_drag_selects_in_shelbi_even_when_the_program_wants_the_mouse() {
        // The program enabled mouse reporting, but Shift+drag is always Shelbi's
        // (selection), never forwarded.
        let mut pane = TerminalPane::new(Size::new(20, 3));
        enable_sgr_mouse(&mut pane);
        pane.view.feed(b"hello world");
        let down = mouse(MouseEventKind::Down(CtMouseButton::Left), 0, 0, KM::SHIFT);
        assert!(
            matches!(pane.on_mouse(&down, 0, 0, Size::new(20, 3)), MouseOutcome::Handled),
            "Shift+press starts a Shelbi selection rather than forwarding to the program"
        );
        let drag = mouse(MouseEventKind::Drag(CtMouseButton::Left), 4, 0, KM::SHIFT);
        assert!(
            matches!(pane.on_mouse(&drag, 4, 0, Size::new(20, 3)), MouseOutcome::Handled),
            "Shift+drag extends the selection, never forwarded"
        );
        // Releasing completes the selection and hands back the copied text.
        let up = mouse(MouseEventKind::Up(CtMouseButton::Left), 4, 0, KM::SHIFT);
        match pane.on_mouse(&up, 4, 0, Size::new(20, 3)) {
            MouseOutcome::Copy(text) => assert_eq!(text, "hello"),
            other => panic!("a finished Shift+drag should copy the selection, got {other:?}"),
        }
    }

    #[test]
    fn a_completed_selection_copies_the_expected_osc52_sequence() {
        // A plain drag (the program is not reporting the mouse) selects "hello"
        // and the release yields the OSC 52 copy sequence: ESC ] 52 ; c ;
        // base64("hello") ST.
        let mut pane = TerminalPane::new(Size::new(20, 3));
        pane.view.feed(b"hello world");
        let down = mouse(MouseEventKind::Down(CtMouseButton::Left), 0, 0, KM::NONE);
        pane.on_mouse(&down, 0, 0, Size::new(20, 3));
        let drag = mouse(MouseEventKind::Drag(CtMouseButton::Left), 4, 0, KM::NONE);
        pane.on_mouse(&drag, 4, 0, Size::new(20, 3));
        let up = mouse(MouseEventKind::Up(CtMouseButton::Left), 4, 0, KM::NONE);
        let text = match pane.on_mouse(&up, 4, 0, Size::new(20, 3)) {
            MouseOutcome::Copy(text) => text,
            other => panic!("a finished drag should copy, got {other:?}"),
        };
        assert_eq!(text, "hello");
        // base64("hello") == "aGVsbG8=".
        assert_eq!(osc52(&text), b"\x1b]52;c;aGVsbG8=\x1b\\".to_vec());
    }

    #[test]
    fn search_finds_text_in_scrollback() {
        let mut pane = TerminalPane::new(Size::new(20, 2));
        for i in 0..10 {
            pane.view.feed(format!("row {i} needle{i}\r\n").as_bytes());
        }
        let count = pane.search("needle");
        assert!(count >= 1, "found at least one match for a scrolled-off term");
        assert!(pane.view.search_current().is_some());
    }
}
