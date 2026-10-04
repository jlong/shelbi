//! `shelbi session attach` — a rendered, full-screen, single-session client.
//!
//! This is the escape hatch that replaces `tmux attach`: a standalone way to
//! use an agent from any terminal. It is **not** a raw byte passthrough. The
//! session is sized for whatever client is active; streaming its raw bytes into
//! a differently sized terminal would wrap and position the cursor by the
//! receiving terminal's geometry, which no filtering can fix. Instead we drive
//! a client-side emulator ([`shelbi_term`]) with the session's replay + live
//! output, and paint its grid into our terminal — clipped or letterboxed when
//! our size differs from the session's (plan, "`shelbi attach <workspace>`").
//!
//! - **Sizing.** On attach, and on every local resize, we report our terminal
//!   size to the session. The session follows the most-recently-active client,
//!   so when we are active it takes our size and fills the terminal; when
//!   another client is active and larger/smaller, [`viewport::fit`] clips or
//!   letterboxes its grid into ours.
//! - **Input.** Keys, mouse, paste, and focus are encoded through
//!   [`shelbi_term::input`] and written to the session; the client emulator
//!   never answers terminal queries (only the session, beside the real PTY,
//!   does).
//! - **Detach.** The configured key (default Ctrl+]) detaches without stopping
//!   the session; a one-line hint shows it for a few seconds after attach.
//! - **Robustness.** The terminal is restored on detach, on child exit, and on
//!   panic (a panic hook plus an RAII guard).

use std::io::{self, Write};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton as CtMouseButton, MouseEvent as CtMouseEvent,
    MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color as RatColor, Modifier, Style};
use ratatui::Terminal;

use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color as VtColor, NamedColor};

use shelbi_client::{Connection, DiscoveredSession, SessionEvent};
use shelbi_proto::capability;
use shelbi_term::input::{self, Key, Modifiers};
use shelbi_term::view::TerminalView;
use shelbi_term::viewport::{self, Placement};
use shelbi_term::Size;

/// Seconds the detach hint stays on the bottom row after attach.
const HINT_SECS: u64 = 4;

/// A parsed detach key: a crossterm key code plus the modifiers that must be
/// held. Matching compares the Ctrl/Alt/Shift/Super bits only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetachKey {
    code: KeyCode,
    mods: KeyModifiers,
}

/// The modifier bits a detach-key match considers.
fn match_mask() -> KeyModifiers {
    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::SUPER
}

/// Parse a detach-key spec like `ctrl-]`, `ctrl-q`, `alt-d`, or `esc`.
///
/// Tokens are split on `-`/`+`; all but the last name modifiers
/// (`ctrl`/`alt`/`shift`/`super` and common aliases), the last names the key (a
/// single character or `enter`/`esc`/`tab`/`space`/`backspace`/`delete`/`fN`).
/// Letters normalize to lowercase so the spec is case-insensitive.
pub fn parse_detach_key(spec: &str) -> Result<DetachKey> {
    let parts: Vec<&str> = spec.split(['-', '+']).collect();
    if parts.iter().any(|p| p.is_empty()) {
        bail!("invalid detach key `{spec}` (empty segment)");
    }
    let (key_tok, mod_toks) = parts
        .split_last()
        .ok_or_else(|| anyhow!("empty detach key"))?;
    let mut mods = KeyModifiers::empty();
    for m in mod_toks {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "c" => mods |= KeyModifiers::CONTROL,
            "alt" | "opt" | "option" | "meta" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            "super" | "cmd" | "command" | "win" => mods |= KeyModifiers::SUPER,
            other => bail!("unknown modifier `{other}` in detach key `{spec}`"),
        }
    }
    Ok(DetachKey {
        code: parse_key_code(key_tok, spec)?,
        mods,
    })
}

fn parse_key_code(tok: &str, spec: &str) -> Result<KeyCode> {
    let low = tok.to_ascii_lowercase();
    Ok(match low.as_str() {
        "enter" | "return" | "cr" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        _ if low.starts_with('f') && low[1..].parse::<u8>().is_ok() => {
            KeyCode::F(low[1..].parse().unwrap())
        }
        _ => {
            let mut chars = tok.chars();
            let c = chars
                .next()
                .ok_or_else(|| anyhow!("empty key in detach key `{spec}`"))?;
            if chars.next().is_some() {
                bail!("unknown key `{tok}` in detach key `{spec}`");
            }
            KeyCode::Char(if c.is_ascii_alphabetic() {
                c.to_ascii_lowercase()
            } else {
                c
            })
        }
    })
}

/// Whether a crossterm key event is the detach key.
fn matches_detach(dk: &DetachKey, ev: &KeyEvent) -> bool {
    // Letters normalize to lowercase so `ctrl-q` matches Ctrl+Q and Ctrl+q.
    let code = match ev.code {
        KeyCode::Char(c) if c.is_ascii_alphabetic() => KeyCode::Char(c.to_ascii_lowercase()),
        other => other,
    };
    let mask = match_mask();
    code == dk.code && (ev.modifiers & mask) == (dk.mods & mask)
}

/// A human label for the hint line, e.g. `Ctrl+]`.
fn describe_detach(dk: &DetachKey) -> String {
    let mut s = String::new();
    if dk.mods.contains(KeyModifiers::CONTROL) {
        s.push_str("Ctrl+");
    }
    if dk.mods.contains(KeyModifiers::ALT) {
        s.push_str("Alt+");
    }
    if dk.mods.contains(KeyModifiers::SHIFT) {
        s.push_str("Shift+");
    }
    if dk.mods.contains(KeyModifiers::SUPER) {
        s.push_str("Super+");
    }
    s.push_str(&describe_code(dk.code));
    s
}

fn describe_code(code: KeyCode) -> String {
    match code {
        KeyCode::Char(' ') => "Space".into(),
        KeyCode::Char(c) => c.to_string().to_uppercase(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Delete => "Delete".into(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}"),
    }
}

// --- the attach client -----------------------------------------------------

/// Attach a rendered client to `session` until the user detaches or the
/// session's child exits. The terminal is always restored on return.
pub fn attach(session: DiscoveredSession, detach: DetachKey) -> Result<()> {
    let (conn, events) = Connection::open(&session.sock, None, capability::ALL)
        .map_err(|e| anyhow!("connecting to session {}: {e}", session.short_id))?;
    let info = conn
        .info()
        .map_err(|e| anyhow!("querying session {}: {e}", session.short_id))?;
    let mut session_size = Size::new(info.cols.max(1), info.rows.max(1));
    let mut view = TerminalView::new(session_size);
    conn.attach(None).map_err(|e| anyhow!("attaching: {e}"))?;

    // Everything below this guard runs with the terminal in raw/alt-screen
    // mode; the guard (and a panic hook) restore it on every exit path.
    let _guard = RawGuard::enter().context("entering raw mode")?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))
        .context("initializing the terminal")?;

    // Claim our size so the session reflows to fill this terminal.
    let (vc, vr) = crossterm::terminal::size().unwrap_or((session_size.cols, session_size.rows));
    let mut viewer = Size::new(vc.max(1), vr.max(1));
    let _ = conn.resize(viewer.cols, viewer.rows);

    let rx = events.into_inner();
    let hint_until = Instant::now() + Duration::from_secs(HINT_SECS);
    let mut dirty = true;
    let mut last_hint = false;
    let mut bell = false;
    let mut exited: Option<shelbi_proto::Exited> = None;
    let mut detached = false;

    'main: loop {
        // Service any pending local input without blocking.
        while event::poll(Duration::ZERO).unwrap_or(false) {
            let ev = match event::read() {
                Ok(ev) => ev,
                Err(_) => break,
            };
            if handle_event(ev, &conn, &mut view, session_size, &mut viewer, &mut dirty, &detach) {
                let _ = conn.detach();
                detached = true;
                break 'main;
            }
        }

        // Wait briefly for session output/events; a disconnect means the
        // session process is gone (e.g. killed without an exit event).
        match rx.recv_timeout(Duration::from_millis(16)) {
            Ok(ev) => {
                apply_session_event(ev, &mut view, &mut session_size, &mut dirty, &mut bell, &mut exited);
                while let Ok(ev) = rx.try_recv() {
                    apply_session_event(
                        ev,
                        &mut view,
                        &mut session_size,
                        &mut dirty,
                        &mut bell,
                        &mut exited,
                    );
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break 'main,
        }

        let show_hint = Instant::now() < hint_until;
        if dirty || show_hint || last_hint {
            draw(&mut term, &view, session_size, show_hint, &detach)?;
            last_hint = show_hint;
            dirty = false;
        }
        if bell {
            let mut out = io::stdout();
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
            bell = false;
        }
        if exited.is_some() {
            break 'main;
        }
    }

    drop(term);
    drop(_guard); // restore the terminal before writing to the normal screen

    if let Some(e) = exited {
        eprintln!("session {} exited{}", session.short_id, describe_exit(&e));
    } else if detached {
        eprintln!("detached from {} ({})", session.short_id, session.meta.name);
    } else {
        eprintln!("session {} ended", session.short_id);
    }
    Ok(())
}

fn describe_exit(e: &shelbi_proto::Exited) -> String {
    match (e.code, e.signal) {
        (Some(c), _) => format!(" (code {c})"),
        (_, Some(s)) => format!(" (signal {s})"),
        _ => String::new(),
    }
}

/// Apply one session event to the client emulator / loop state.
fn apply_session_event(
    ev: SessionEvent,
    view: &mut TerminalView,
    session_size: &mut Size,
    dirty: &mut bool,
    bell: &mut bool,
    exited: &mut Option<shelbi_proto::Exited>,
) {
    match ev {
        SessionEvent::Resync { replay, .. } => {
            view.feed(&replay);
            *dirty = true;
        }
        SessionEvent::Output { data, .. } => {
            view.feed(&data);
            *dirty = true;
        }
        SessionEvent::Resized { cols, rows, .. } | SessionEvent::SizeChanged { cols, rows } => {
            *session_size = Size::new(cols.max(1), rows.max(1));
            view.resize(*session_size);
            *dirty = true;
        }
        SessionEvent::Title(t) => {
            let _ = execute!(io::stdout(), SetTitle(t));
        }
        SessionEvent::Bell => *bell = true,
        SessionEvent::Exited(e) => {
            *exited = Some(e);
            *dirty = true;
        }
    }
}

/// Handle one local input event. Returns `true` when the user pressed the
/// detach key (the caller then detaches and exits).
fn handle_event(
    ev: Event,
    conn: &Connection,
    view: &mut TerminalView,
    session_size: Size,
    viewer: &mut Size,
    dirty: &mut bool,
    detach: &DetachKey,
) -> bool {
    match ev {
        Event::Key(k) => {
            // Releases are only emitted under the kitty protocol; forward key
            // presses (and auto-repeats) to the PTY.
            if matches!(k.kind, KeyEventKind::Release) {
                return false;
            }
            if matches_detach(detach, &k) {
                return true;
            }
            if let Some((key, mods)) = map_key(&k) {
                let enc = key_encoding(view.emulator().mode());
                let bytes = input::encode_key(key, mods, enc);
                if !bytes.is_empty() {
                    view.on_user_input();
                    *dirty = true;
                    let _ = conn.input(&bytes);
                }
            }
        }
        Event::Paste(s) => {
            view.on_user_input();
            *dirty = true;
            let _ = conn.paste(&s);
        }
        Event::Resize(c, r) => {
            *viewer = Size::new(c.max(1), r.max(1));
            let _ = conn.resize(viewer.cols, viewer.rows);
            *dirty = true;
        }
        Event::FocusGained => {
            if let Some(b) = input::encode_focus(true, view.emulator().focus_reporting()) {
                let _ = conn.input(&b);
            }
        }
        Event::FocusLost => {
            if let Some(b) = input::encode_focus(false, view.emulator().focus_reporting()) {
                let _ = conn.input(&b);
            }
        }
        Event::Mouse(m) => handle_mouse(m, conn, view, session_size, *viewer, dirty),
    }
    false
}

/// Derive the key-encoding modes from the emulator's current terminal mode.
fn key_encoding(mode: TermMode) -> input::KeyEncoding {
    input::KeyEncoding {
        kitty: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
        application_cursor_keys: mode.contains(TermMode::APP_CURSOR),
        newline_mode: mode.contains(TermMode::LINE_FEED_NEW_LINE),
    }
}

fn map_key(k: &KeyEvent) -> Option<(Key, Modifiers)> {
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
    // A plain typed character already encodes its shift (`A`, `!`); passing a
    // lone Shift modifier too would make the kitty encoder emit a modified key.
    if matches!(key, Key::Char(_)) && mods.shift && !mods.ctrl && !mods.alt && !mods.sup {
        mods.shift = false;
    }
    Some((key, mods))
}

fn handle_mouse(
    m: CtMouseEvent,
    conn: &Connection,
    view: &mut TerminalView,
    session_size: Size,
    viewer: Size,
    dirty: &mut bool,
) {
    let mode = view.emulator().mode();
    let reporting = mode.intersects(TermMode::MOUSE_MODE);
    let shift = m.modifiers.contains(KeyModifiers::SHIFT);
    match input::mouse_owner(reporting, shift) {
        input::MouseOwner::Shelbi => match m.kind {
            // Shelbi owns the wheel for scrollback (a no-op on the alt screen,
            // which `TerminalView` enforces).
            MouseEventKind::ScrollUp => {
                view.scroll_up(3);
                *dirty = true;
            }
            MouseEventKind::ScrollDown => {
                view.scroll_down(3);
                *dirty = true;
            }
            _ => {}
        },
        input::MouseOwner::Program => {
            if let Some(ev) = map_mouse(&m) {
                let modes = input::MouseModes {
                    reporting,
                    sgr: mode.contains(TermMode::SGR_MOUSE),
                };
                let placement = viewport::fit(session_size, viewer);
                if let Some(bytes) = input::encode_mouse(&ev, modes, &placement) {
                    let _ = conn.input(&bytes);
                }
            }
        }
    }
}

fn map_mouse(m: &CtMouseEvent) -> Option<input::MouseEvent> {
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
        col: m.column,
        row: m.row,
        mods,
    })
}

// --- rendering -------------------------------------------------------------

fn draw(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    view: &TerminalView,
    session_size: Size,
    hint: bool,
    detach: &DetachKey,
) -> Result<()> {
    term.draw(|frame| {
        let area = frame.area();
        let viewer = Size::new(area.width, area.height);
        let placement = viewport::fit(session_size, viewer);
        let cursor = render_grid(frame.buffer_mut(), area, view, &placement);
        if hint {
            render_hint(frame.buffer_mut(), area, detach);
        }
        if let Some((x, y)) = cursor {
            frame.set_cursor_position(Position::new(x, y));
        }
    })
    .context("drawing the session")?;
    Ok(())
}

/// Paint the session's visible grid into `buf` over `area`, clipping or
/// letterboxing per `placement`. Returns the viewer cursor position when the
/// cursor is visible (shown) and inside the window.
fn render_grid(
    buf: &mut Buffer,
    area: Rect,
    view: &TerminalView,
    placement: &Placement,
) -> Option<(u16, u16)> {
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

    for indexed in grid.display_iter() {
        let srow = indexed.point.line.0 + offset;
        if srow < 0 {
            continue;
        }
        let (srow, scol) = (srow as u32, indexed.point.column.0 as u32);
        // Clip window: `src_start` is always 0, so a session cell is visible
        // only while it is inside `len` on each axis.
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
            // The wide glyph in the neighbouring cell owns both columns; keep
            // this one empty so ratatui preserves the alignment.
            out.set_symbol("");
            continue;
        }
        out.set_char(if cell.c == '\0' { ' ' } else { cell.c });
        out.set_style(cell_style(cell));
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

/// Translate an alacritty cell's colors and flags into a ratatui style. Named
/// and indexed colors pass through so the user's own terminal palette applies;
/// only explicit RGB is pinned.
fn cell_style(cell: &Cell) -> Style {
    let (mut fg, mut bg) = (conv_color(cell.fg), conv_color(cell.bg));
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

fn conv_color(c: VtColor) -> RatColor {
    match c {
        VtColor::Named(n) => named_to_rat(n),
        VtColor::Spec(rgb) => RatColor::Rgb(rgb.r, rgb.g, rgb.b),
        VtColor::Indexed(i) => RatColor::Indexed(i),
    }
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
        // Default fg/bg/cursor defer to the real terminal's defaults.
        Foreground | Background | Cursor | BrightForeground | DimForeground => RatColor::Reset,
    }
}

/// Paint the one-line detach hint along the bottom row, reversed so it reads
/// over any content. Shown only for the first few seconds after attach.
fn render_hint(buf: &mut Buffer, area: Rect, detach: &DetachKey) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let label = format!(" detach: {} ", describe_detach(detach));
    let y = area.bottom() - 1;
    let style = Style::default().add_modifier(Modifier::REVERSED);
    for (x, ch) in (area.left()..area.right()).zip(label.chars()) {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

// --- terminal lifecycle ----------------------------------------------------

/// Restore the terminal to its pre-attach state. Idempotent and best-effort so
/// it is safe from a panic hook, the RAII guard, and an explicit call.
fn restore_terminal() {
    let mut out = io::stdout();
    let _ = execute!(
        out,
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show,
    );
    let _ = disable_raw_mode();
    let _ = out.flush();
}

/// RAII terminal setup: raw mode, alternate screen, mouse/focus/paste capture,
/// and a best-effort keyboard-enhancement push so Shift+Enter reaches the
/// agent. Restores everything on drop, and installs a panic hook that restores
/// first so a panic never leaves the terminal wedged.
struct RawGuard;

impl RawGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableFocusChange,
            EnableBracketedPaste,
        )?;
        // Best-effort: terminals that don't support this ignore the escape.
        let _ = execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            prev(info);
        }));
        Ok(RawGuard)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(c: char, fg: VtColor, bg: VtColor, flags: Flags) -> Cell {
        Cell {
            c,
            fg,
            bg,
            flags,
            extra: None,
        }
    }

    #[test]
    fn parse_detach_key_handles_modifiers_and_named_keys() {
        let d = parse_detach_key("ctrl-]").unwrap();
        assert_eq!(d.code, KeyCode::Char(']'));
        assert_eq!(d.mods, KeyModifiers::CONTROL);

        let d = parse_detach_key("Ctrl-Q").unwrap();
        assert_eq!(d.code, KeyCode::Char('q'), "letters normalize to lowercase");
        assert_eq!(d.mods, KeyModifiers::CONTROL);

        let d = parse_detach_key("alt+d").unwrap();
        assert_eq!(d.code, KeyCode::Char('d'));
        assert_eq!(d.mods, KeyModifiers::ALT);

        assert_eq!(parse_detach_key("esc").unwrap().code, KeyCode::Esc);
        assert_eq!(parse_detach_key("f5").unwrap().code, KeyCode::F(5));

        assert!(parse_detach_key("").is_err());
        assert!(parse_detach_key("ctrl-").is_err());
        assert!(parse_detach_key("bogus-x").is_err());
        assert!(parse_detach_key("ctrl-ab").is_err());
    }

    #[test]
    fn matches_detach_compares_code_and_modifiers() {
        let d = parse_detach_key("ctrl-]").unwrap();
        assert!(matches_detach(
            &d,
            &KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL)
        ));
        // Wrong modifiers / wrong key do not match.
        assert!(!matches_detach(&d, &KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE)));
        assert!(!matches_detach(
            &d,
            &KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)
        ));

        // A `ctrl-q` spec matches both Ctrl+Q and Ctrl+q.
        let q = parse_detach_key("ctrl-q").unwrap();
        assert!(matches_detach(&q, &KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::CONTROL)));
        assert!(matches_detach(&q, &KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn describe_detach_is_human_readable() {
        assert_eq!(describe_detach(&parse_detach_key("ctrl-]").unwrap()), "Ctrl+]");
        assert_eq!(describe_detach(&parse_detach_key("alt-d").unwrap()), "Alt+D");
        assert_eq!(describe_detach(&parse_detach_key("esc").unwrap()), "Esc");
    }

    #[test]
    fn cell_style_maps_colors_and_flags() {
        let s = cell_style(&cell(
            'x',
            VtColor::Named(NamedColor::Red),
            VtColor::Spec(alacritty_terminal::vte::ansi::Rgb { r: 1, g: 2, b: 3 }),
            Flags::BOLD | Flags::UNDERLINE,
        ));
        assert_eq!(s.fg, Some(RatColor::Red));
        assert_eq!(s.bg, Some(RatColor::Rgb(1, 2, 3)));
        assert!(s.add_modifier.contains(Modifier::BOLD));
        assert!(s.add_modifier.contains(Modifier::UNDERLINED));

        // INVERSE swaps fg and bg.
        let s = cell_style(&cell(
            'x',
            VtColor::Named(NamedColor::Red),
            VtColor::Named(NamedColor::Blue),
            Flags::INVERSE,
        ));
        assert_eq!(s.fg, Some(RatColor::Blue));
        assert_eq!(s.bg, Some(RatColor::Red));
    }

    /// The acceptance case: the same session grid renders correctly into two
    /// viewers of different width — a wider one letterboxes (centered, blank
    /// margins), a narrower one clips (anchored top-left, excess dropped).
    #[test]
    fn two_viewers_at_different_widths_render_clipped_and_letterboxed() {
        // A 10x3 session with "ABCDEFGHIJ" on the top row.
        let session = Size::new(10, 3);
        let mut view = TerminalView::new(session);
        view.feed(b"ABCDEFGHIJ");

        // Wider viewer (14x5): letterbox. cols surplus 4 -> pad_before 2;
        // rows surplus 2 -> pad_before 1. 'A' lands at (2, 1); margins blank.
        let wide = Size::new(14, 5);
        let area = Rect::new(0, 0, wide.cols, wide.rows);
        let mut buf = Buffer::empty(area);
        let placement = viewport::fit(session, wide);
        assert!(placement.is_letterboxed());
        render_grid(&mut buf, area, &view, &placement);
        assert_eq!(buf.cell((2, 1)).unwrap().symbol(), "A");
        assert_eq!(buf.cell((11, 1)).unwrap().symbol(), "J");
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), " ", "top margin is blank");
        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), " ", "left margin is blank");
        assert_eq!(buf.cell((13, 4)).unwrap().symbol(), " ", "bottom-right margin is blank");

        // Narrower viewer (6x2): clip. Only the first 6 columns, anchored at
        // the origin; columns 6..10 are dropped.
        let narrow = Size::new(6, 2);
        let area = Rect::new(0, 0, narrow.cols, narrow.rows);
        let mut buf = Buffer::empty(area);
        let placement = viewport::fit(session, narrow);
        assert!(placement.is_clipped());
        render_grid(&mut buf, area, &view, &placement);
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "A");
        assert_eq!(buf.cell((5, 0)).unwrap().symbol(), "F");
        // Column 6+ does not exist in this viewer (clipped).
        assert!(buf.cell((6, 0)).is_none());
    }
}
