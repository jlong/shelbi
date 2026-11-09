//! Input encoding: keys, mouse, paste, and focus into PTY bytes.
//!
//! This turns a UI's input events into the raw bytes an
//! [`Input`](shelbi_proto::Input) frame carries (or the text a
//! [`Paste`](shelbi_proto::Paste) frame carries). The public types here are
//! toolkit-neutral: a crossterm TUI and a gpui app each map their native
//! events onto [`Key`], [`Modifiers`], and [`MouseEvent`], so no crossterm or
//! gpui type crosses this boundary.
//!
//! Key encoding is this crate's own table ([`encode_key`]): legacy/xterm
//! sequences, application cursor keys (DECCKM), newline mode (LNM), and the
//! CSI-u form of the kitty keyboard protocol. It replaced termwiz's encoder
//! and is pinned byte-for-byte to it by the golden table in
//! `tests/fixtures/key_encoding_golden.tsv` (see
//! `tests/key_encoding_golden.rs`), quirks included: where the two could
//! disagree, the table is what programs already saw.
//! termwiz is MIT (Copyright (c) 2018 Wez Furlong), and its license covers
//! this port: Acknowledgements keeps crediting it as a ported crate
//! (`shelbi_app::licenses::SHARED_PORTED`).
//!
//! ## Kitty keyboard protocol
//!
//! When the program has enabled the kitty keyboard protocol, keys are encoded
//! in the CSI-u form its progressive-enhancement layer consumes. The case that
//! forces this is Claude Code's **Shift+Enter**: with the protocol on it must
//! encode as `ESC[13;2u` (a distinct, reportable key event), where a legacy
//! terminal would send a bare `\r` indistinguishable from plain Enter. See
//! [`encode_key`] and its tests.
//!
//! ## Mouse ownership
//!
//! [`mouse_owner`] is the pure policy that decides, for a mouse event, whether
//! it belongs to the program or to Shelbi (scrollback / selection):
//!
//! - the program gets wheel, click, and drag **only** when it enabled mouse
//!   reporting;
//! - Shift+wheel and Shift+drag are **always** Shelbi's;
//! - with no program mouse mode, plain wheel and drag are Shelbi's.

use std::fmt::Write as _;

use crate::viewport::Placement;

/// Keyboard modifiers in play for a key or mouse event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Alt / Option.
    pub alt: bool,
    /// Control.
    pub ctrl: bool,
    /// Super / Command / Windows.
    pub sup: bool,
}

impl Modifiers {
    /// No modifiers.
    pub const NONE: Modifiers = Modifiers { shift: false, alt: false, ctrl: false, sup: false };
    /// Shift only.
    pub const SHIFT: Modifiers = Modifiers { shift: true, alt: false, ctrl: false, sup: false };
    /// Control only.
    pub const CTRL: Modifiers = Modifiers { shift: false, alt: false, ctrl: true, sup: false };
    /// Alt only.
    pub const ALT: Modifiers = Modifiers { shift: false, alt: true, ctrl: false, sup: false };

    fn is_empty(self) -> bool {
        self == Modifiers::NONE
    }

    /// Shift, Alt, or Control is held (the modifiers an xterm sequence can
    /// carry; Super alone leaves a navigation key unmodified).
    fn any_xterm(self) -> bool {
        self.shift || self.alt || self.ctrl
    }

    /// The xterm modifier parameter, `1 + shift + 2*alt + 4*ctrl`. Super is
    /// deliberately not encoded (xterm's own `+8` for Meta is never sent):
    /// termwiz never encoded it, and the golden table pins that.
    fn xterm_param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}

/// A toolkit-neutral key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character key (letters, digits, punctuation, space).
    Char(char),
    /// Return / Enter.
    Enter,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Escape.
    Escape,
    /// Delete (forward delete).
    Delete,
    /// Insert.
    Insert,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// A function key F1..=F24.
    Function(u8),
}

/// The terminal modes that change how a key is encoded. A caller derives these
/// from the emulator's current mode (see
/// [`KeyEncoding::from_term_mode`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyEncoding {
    /// The program enabled the kitty keyboard protocol: encode in CSI-u form
    /// (so Shift+Enter, Ctrl+I vs Tab, etc. are distinguishable).
    pub kitty: bool,
    /// Application cursor keys (DECCKM): arrows send `ESC O A` not `ESC [ A`.
    pub application_cursor_keys: bool,
    /// Line-feed/new-line mode (LNM): Enter sends CR+LF.
    pub newline_mode: bool,
}

/// Encode a key press into the bytes to write to the PTY.
///
/// Returns an empty vector for a key that has no encoding (a function key
/// outside F1..=F24).
///
/// The rules, in the order they are tried:
///
/// - **Normalize.** Shift+a-z becomes A-Z; Shift is then dropped from an
///   uppercase letter or ASCII punctuation (the character already carries
///   it). `Char('\x7f')` is Delete and `Char('\x08')` is Backspace.
/// - **Characters.** Ctrl+letter, Ctrl+uppercase, and (under kitty) Ctrl on a
///   key that is ambiguous as a C0 byte (`i m [ { @`) become CSI-u
///   `ESC[{codepoint};{m}u`, falling back to the C0 byte outside kitty.
///   Otherwise Ctrl maps to its C0 byte, Alt prefixes ESC, and any other
///   modified character is CSI-u under kitty and the bare character outside it.
/// - **Enter / Escape / Backspace.** Bare bytes (`\r`, `\r\n` under LNM,
///   ESC, DEL) with an ESC prefix for Alt; with Shift or Ctrl they are CSI-u
///   under kitty (Ctrl+Backspace is BS).
/// - **Tab.** `\t`, Shift+Tab `ESC[Z`, Ctrl+Tab `ESC[9;5u`, Ctrl+Shift+Tab
///   `ESC[1;5Z`, Alt prefixes ESC; unmodified Tab under kitty is `ESC[9u`.
/// - **Navigation.** Arrows, Home (`H`), and End (`F`) are `ESC[x` (`ESC O x`
///   under DECCKM) and `ESC[1;{m}x` when modified; Insert, Delete, PgUp, and
///   PgDn are `ESC[{2,3,5,6}~` and `ESC[{n};{m}~`.
/// - **Function keys.** F1-F4 are SS3 `P`-`S` (`ESC[1;{m}P` modified); F5-F24
///   are `ESC[{n}~` / `ESC[{n};{m}~` with xterm's key numbers.
///
/// `m` is the xterm modifier parameter, `1 + shift + 2*alt + 4*ctrl`.
pub fn encode_key(key: Key, mods: Modifiers, encoding: KeyEncoding) -> Vec<u8> {
    // Under the kitty keyboard protocol, the unmodified Tab *key* is the CSI-u
    // event `ESC[9u` (9 = HT's codepoint). A bare `\t` there is read as literal
    // tab *text*, not a Tab key press, by a program that enabled the protocol
    // (Claude Code does, pushing `CSI > 5 u`), so completion (file paths,
    // slash commands) would never fire. Modified Tab keeps its legacy forms:
    // Shift+Tab stays the backtab `ESC[Z`, which Claude Code maps to its
    // Shift+Tab mode cycle (the CSI-u `ESC[9;2u` does not drive it).
    if encoding.kitty && key == Key::Tab && mods == Modifiers::NONE {
        return b"\x1b[9u".to_vec();
    }

    // Normalize Shift onto the character, then drop it where the character
    // already says it (uppercase letters and punctuation).
    let mut mods = mods;
    let key = match key {
        Key::Char(c) => {
            let c = if mods.shift { c.to_ascii_uppercase() } else { c };
            if mods.shift && (c.is_ascii_uppercase() || c.is_ascii_punctuation()) {
                mods.shift = false;
            }
            match c {
                '\x7f' => Key::Delete,
                '\x08' => Key::Backspace,
                c => Key::Char(c),
            }
        }
        other => other,
    };

    let mut buf = String::new();
    let alt_prefix = |buf: &mut String| {
        if mods.alt {
            buf.push('\x1b');
        }
    };
    match key {
        Key::Char(c) => {
            let ambiguous = matches!(c, 'i' | 'I' | 'm' | 'M' | '[' | '{' | '@');
            if mods.ctrl && (c.is_ascii_uppercase() || (ambiguous && encoding.kitty)) {
                csi_u(&mut buf, c, mods, encoding);
            } else if let Some(c0) = ctrl_byte(c).filter(|_| mods.ctrl) {
                alt_prefix(&mut buf);
                buf.push(c0);
            } else if mods.alt && (c.is_ascii_alphanumeric() || c.is_ascii_punctuation()) {
                buf.push('\x1b');
                buf.push(c);
            } else if mods.is_empty() {
                buf.push(c);
            } else {
                csi_u(&mut buf, c, mods, encoding);
            }
        }
        Key::Backspace => {
            // Backspace sends DEL (the default VERASE); BS only with Ctrl.
            if mods.ctrl {
                csi_u(&mut buf, '\x08', mods, encoding);
            } else if mods.shift {
                csi_u(&mut buf, '\x7f', mods, encoding);
            } else {
                alt_prefix(&mut buf);
                buf.push('\x7f');
            }
        }
        Key::Enter | Key::Escape => {
            let c = if key == Key::Enter { '\r' } else { '\x1b' };
            if mods.shift || mods.ctrl {
                csi_u(&mut buf, c, mods, encoding);
            } else {
                alt_prefix(&mut buf);
                buf.push(c);
                if key == Key::Enter && encoding.newline_mode {
                    buf.push('\n');
                }
            }
        }
        Key::Tab => {
            alt_prefix(&mut buf);
            let rest = Modifiers { alt: false, ..mods };
            buf.push_str(if rest == Modifiers::CTRL {
                "\x1b[9;5u"
            } else if rest == (Modifiers { shift: true, ..Modifiers::CTRL }) {
                "\x1b[1;5Z"
            } else if rest == Modifiers::SHIFT {
                "\x1b[Z"
            } else {
                "\t"
            });
        }
        Key::Up | Key::Down | Key::Right | Key::Left | Key::Home | Key::End => {
            let c = match key {
                Key::Up => 'A',
                Key::Down => 'B',
                Key::Right => 'C',
                Key::Left => 'D',
                Key::Home => 'H',
                _ => 'F',
            };
            if mods.any_xterm() {
                let _ = write!(buf, "\x1b[1;{}{c}", mods.xterm_param());
            } else if encoding.application_cursor_keys {
                let _ = write!(buf, "\x1bO{c}");
            } else {
                let _ = write!(buf, "\x1b[{c}");
            }
        }
        Key::Insert | Key::Delete | Key::PageUp | Key::PageDown => {
            let n = match key {
                Key::Insert => 2,
                Key::Delete => 3,
                Key::PageUp => 5,
                _ => 6,
            };
            if mods.any_xterm() {
                let _ = write!(buf, "\x1b[{n};{}~", mods.xterm_param());
            } else {
                let _ = write!(buf, "\x1b[{n}~");
            }
        }
        Key::Function(n @ 1..=4) => {
            let c = char::from(b'P' + (n - 1));
            if mods.is_empty() {
                let _ = write!(buf, "\x1bO{c}");
            } else {
                let _ = write!(buf, "\x1b[1;{}{c}", mods.xterm_param());
            }
        }
        Key::Function(n @ 5..=24) => {
            // The VT220-style key numbers, with their gaps (16, 22, 27, 30, 35-41).
            const CODES: [u8; 20] = [15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34, 42, 43, 44, 45];
            let code = CODES[usize::from(n - 5)];
            if mods.any_xterm() {
                let _ = write!(buf, "\x1b[{code};{}~", mods.xterm_param());
            } else {
                let _ = write!(buf, "\x1b[{code}~");
            }
        }
        Key::Function(_) => {}
    }
    buf.into_bytes()
}

/// A character that carries modifiers the legacy encodings can't express.
/// Under kitty an ASCII character is the CSI-u event `ESC[{codepoint};{m}u`;
/// otherwise Ctrl folds to the C0 byte where one exists and Alt prefixes ESC.
fn csi_u(buf: &mut String, c: char, mods: Modifiers, encoding: KeyEncoding) {
    if encoding.kitty && c.is_ascii() {
        let _ = write!(buf, "\x1b[{};{}u", u32::from(c), mods.xterm_param());
        return;
    }
    let c = ctrl_byte(c).filter(|_| mods.ctrl).unwrap_or(c);
    if mods.alt {
        buf.push('\x1b');
    }
    buf.push(c);
}

/// The C0 control byte Ctrl+`c` sends on a US layout, where there is one
/// (including the digit-row aliases `2`-`8` that xterm honors).
fn ctrl_byte(c: char) -> Option<char> {
    Some(match c {
        '@' | '`' | ' ' | '2' => '\x00',
        'a'..='z' => char::from(c as u8 - b'a' + 1),
        'A'..='Z' => char::from(c as u8 - b'A' + 1),
        '[' | '3' | '{' => '\x1b',
        '\\' | '4' | '|' => '\x1c',
        ']' | '5' | '}' => '\x1d',
        '^' | '6' | '~' => '\x1e',
        '_' | '7' | '/' => '\x1f',
        '8' | '?' => '\x7f',
        _ => return None,
    })
}

/// Wrap `text` for delivery to the PTY. With `bracketed` set (the program
/// enabled bracketed paste, `ESC[?2004h`), the text is framed with the paste
/// markers and any embedded end marker is neutralized so the payload can't
/// close the bracket early; otherwise the raw bytes are sent.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return text.as_bytes().to_vec();
    }
    // Strip any embedded paste-end marker so pasted content cannot smuggle
    // itself out of bracketed-paste mode and be run as keystrokes.
    let cleaned = text.replace("\x1b[201~", "");
    let mut out = Vec::with_capacity(cleaned.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(cleaned.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// The text a drop of files onto a terminal pastes: each path with the
/// characters a shell would split or expand backslash-escaped, joined by
/// spaces and followed by one more space, as Terminal.app and Ghostty paste a
/// dropped file (so the user can keep typing after it). A terminal-hosted
/// client (the TUI) receives exactly that paste from its terminal; the desktop
/// app builds it here so a drop reads the same to the program (Claude Code
/// attaches an image from such a path). Non-UTF-8 paths are converted lossily.
pub fn dropped_paths_text<P: AsRef<std::path::Path>>(paths: &[P]) -> String {
    paths
        .iter()
        .map(|p| {
            let raw = p.as_ref().to_string_lossy();
            let mut out = String::with_capacity(raw.len());
            for c in raw.chars() {
                if !(c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | ',' | ':' | '@' | '%' | '=')) {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push(' ');
            out
        })
        .collect()
}

/// Encode a focus change, if the program enabled focus reporting
/// (`ESC[?1004h`). Focus in is `ESC[I`, focus out is `ESC[O`. Returns `None`
/// when the program is not watching focus, so nothing is sent.
pub fn encode_focus(focused: bool, focus_reporting: bool) -> Option<Vec<u8>> {
    if !focus_reporting {
        return None;
    }
    Some(if focused { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() })
}

/// Who a mouse event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseOwner {
    /// Forward it to the program (encode and write to the PTY).
    Program,
    /// Shelbi handles it locally (scrollback / selection).
    Shelbi,
}

/// The pure mouse-ownership policy (see the module docs).
///
/// `program_reporting` is whether the program enabled any mouse reporting mode;
/// `shift` is whether Shift is held. Shift always wins for Shelbi; otherwise
/// the program gets the event only if it asked for the mouse.
pub fn mouse_owner(program_reporting: bool, shift: bool) -> MouseOwner {
    if shift || !program_reporting {
        MouseOwner::Shelbi
    } else {
        MouseOwner::Program
    }
}

/// A mouse button or wheel direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Left button.
    Left,
    /// Middle button.
    Middle,
    /// Right button.
    Right,
    /// Wheel scrolled up.
    WheelUp,
    /// Wheel scrolled down.
    WheelDown,
}

/// What happened to the button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Button pressed.
    Press,
    /// Button released.
    Release,
    /// Pointer moved with the button held.
    Drag,
    /// Pointer moved with no button held (reported only in any-event motion
    /// mode, `ESC[?1003h`; the event's button is ignored).
    Move,
}

/// A mouse event in **viewer** cell coordinates. The encoder translates the
/// coordinates into the session's pane via a [`Placement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    /// What happened.
    pub action: MouseAction,
    /// Which button or wheel direction.
    pub button: MouseButton,
    /// Viewer column (0-based).
    pub col: u16,
    /// Viewer row (0-based).
    pub row: u16,
    /// Modifiers held.
    pub mods: Modifiers,
}

/// The program's mouse-reporting mode flags, derived from the emulator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseModes {
    /// Any mouse reporting is on (click, drag, or motion).
    pub reporting: bool,
    /// Motion with a button held is reported: button-event tracking
    /// (`ESC[?1002h`) or any-event tracking (`ESC[?1003h`). Plain click
    /// reporting (`ESC[?1000h`) reports presses and releases only.
    pub drag: bool,
    /// Motion with no button held is reported too: any-event tracking
    /// (`ESC[?1003h`).
    pub motion: bool,
    /// SGR extended encoding (`ESC[?1006h`). When off, legacy X10 is used.
    pub sgr: bool,
}

/// Encode a mouse event for the program, translating viewer coordinates to the
/// session pane via `placement`.
///
/// Returns `None` when the program is not reporting the mouse, when it
/// isn't reporting this kind of event (a drag under click-only reporting,
/// motion without any-event tracking), or when the event lands in letterbox
/// margin / outside a clipped window (there is no session cell under it). The ownership decision ([`mouse_owner`]) is the
/// caller's; this only encodes events already destined for the program.
pub fn encode_mouse(ev: &MouseEvent, modes: MouseModes, placement: &Placement) -> Option<Vec<u8>> {
    let wanted = match ev.action {
        MouseAction::Press | MouseAction::Release => modes.reporting,
        MouseAction::Drag => modes.drag,
        MouseAction::Move => modes.motion,
    };
    if !wanted {
        return None;
    }
    let (scol, srow) = placement.viewer_to_session(ev.col, ev.row)?;

    let mut cb = match ev.action {
        // No button held: button code 3, as a release would be.
        MouseAction::Move => 3,
        _ => button_code(ev.button),
    };
    if matches!(ev.action, MouseAction::Drag | MouseAction::Move) {
        cb += 32; // motion bit
    }
    // Modifier bits (Shelbi owns Shift, so it is not forwarded here).
    if ev.mods.alt {
        cb += 8;
    }
    if ev.mods.ctrl {
        cb += 16;
    }

    let released = matches!(ev.action, MouseAction::Release);

    if modes.sgr {
        // SGR: 0-based button, 1-based coordinates, 'M' press / 'm' release.
        let final_byte = if released { 'm' } else { 'M' };
        Some(format!("\x1b[<{};{};{}{}", cb, scol + 1, srow + 1, final_byte).into_bytes())
    } else {
        // Legacy X10: a release is button 3; coordinates and button are offset
        // by 32 and capped at the 223-cell addressable range.
        let legacy_cb = if released { 3 + (cb & !0b11) } else { cb };
        let bb = (legacy_cb as u16 + 32).min(255) as u8;
        let cx = (scol.min(222) + 33) as u8;
        let cy = (srow.min(222) + 33) as u8;
        Some(vec![0x1b, b'[', b'M', bb, cx, cy])
    }
}

fn button_code(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewport::fit;
    use crate::Size;

    // ---- key encoding ----

    #[test]
    fn plain_enter_is_carriage_return() {
        assert_eq!(encode_key(Key::Enter, Modifiers::NONE, KeyEncoding::default()), b"\r");
    }

    #[test]
    fn shift_enter_without_kitty_collapses_to_plain_enter() {
        // Without the kitty protocol there is no CSI-u form, so Shift+Enter
        // degrades to a bare carriage return: a legacy terminal program cannot
        // tell it apart from plain Enter. This is exactly why Claude Code needs
        // the protocol (contrast the next test).
        let enc = KeyEncoding { kitty: false, ..Default::default() };
        assert_eq!(encode_key(Key::Enter, Modifiers::SHIFT, enc), b"\r");
    }

    #[test]
    fn shift_enter_under_kitty_is_csi_u() {
        // The headline case: Claude Code's Shift+Enter under the kitty keyboard
        // protocol must encode as ESC[13;2u.
        let enc = KeyEncoding { kitty: true, ..Default::default() };
        assert_eq!(encode_key(Key::Enter, Modifiers::SHIFT, enc), b"\x1b[13;2u");
    }

    #[test]
    fn plain_enter_under_kitty_is_still_carriage_return() {
        let enc = KeyEncoding { kitty: true, ..Default::default() };
        assert_eq!(encode_key(Key::Enter, Modifiers::NONE, enc), b"\r");
    }

    #[test]
    fn plain_tab_without_kitty_is_ht() {
        // A legacy program (no kitty protocol) expects a bare HT for Tab.
        assert_eq!(encode_key(Key::Tab, Modifiers::NONE, KeyEncoding::default()), b"\t");
    }

    #[test]
    fn plain_tab_under_kitty_is_csi_u() {
        // The headline fix: once a program enables the kitty keyboard protocol
        // (Claude Code pushes `CSI > 5 u`), the Tab *key* must arrive as the
        // CSI-u event ESC[9u. A bare `\t` is read as literal tab text there, so
        // completion never fires.
        let enc = KeyEncoding { kitty: true, ..Default::default() };
        assert_eq!(encode_key(Key::Tab, Modifiers::NONE, enc), b"\x1b[9u");
    }

    #[test]
    fn shift_tab_is_legacy_backtab_in_both_modes() {
        // Shift+Tab stays the legacy backtab ESC[Z with or without the protocol:
        // that is what Claude Code maps to its Shift+Tab mode cycle (the CSI-u
        // ESC[9;2u form does not drive it). The kitty carve-out above is scoped
        // to the *unmodified* Tab so this encoding is untouched.
        assert_eq!(encode_key(Key::Tab, Modifiers::SHIFT, KeyEncoding::default()), b"\x1b[Z");
        let enc = KeyEncoding { kitty: true, ..Default::default() };
        assert_eq!(encode_key(Key::Tab, Modifiers::SHIFT, enc), b"\x1b[Z");
    }

    #[test]
    fn arrows_follow_application_cursor_keys_mode() {
        let normal = KeyEncoding::default();
        assert_eq!(encode_key(Key::Up, Modifiers::NONE, normal), b"\x1b[A");
        let app = KeyEncoding { application_cursor_keys: true, ..Default::default() };
        assert_eq!(encode_key(Key::Up, Modifiers::NONE, app), b"\x1bOA");
    }

    #[test]
    fn ctrl_c_is_etx() {
        assert_eq!(encode_key(Key::Char('c'), Modifiers::CTRL, KeyEncoding::default()), b"\x03");
    }

    #[test]
    fn plain_char_is_the_byte() {
        assert_eq!(encode_key(Key::Char('a'), Modifiers::NONE, KeyEncoding::default()), b"a");
    }

    // ---- paste ----

    #[test]
    fn paste_raw_without_bracketing() {
        assert_eq!(encode_paste("hi there", false), b"hi there");
    }

    #[test]
    fn paste_bracketed_wraps_with_markers() {
        assert_eq!(encode_paste("hi", true), b"\x1b[200~hi\x1b[201~".to_vec());
    }

    #[test]
    fn paste_bracketed_strips_embedded_end_marker() {
        let out = encode_paste("evil\x1b[201~rm -rf", true);
        assert_eq!(out, b"\x1b[200~evilrm -rf\x1b[201~".to_vec());
    }

    // ---- focus ----

    #[test]
    fn focus_is_silent_when_not_reporting() {
        assert_eq!(encode_focus(true, false), None);
        assert_eq!(encode_focus(false, false), None);
    }

    #[test]
    fn focus_in_and_out_sequences() {
        assert_eq!(encode_focus(true, true), Some(b"\x1b[I".to_vec()));
        assert_eq!(encode_focus(false, true), Some(b"\x1b[O".to_vec()));
    }

    // ---- mouse policy ----

    #[test]
    fn shift_always_belongs_to_shelbi() {
        assert_eq!(mouse_owner(true, true), MouseOwner::Shelbi);
        assert_eq!(mouse_owner(false, true), MouseOwner::Shelbi);
    }

    #[test]
    fn program_gets_the_mouse_only_when_reporting() {
        assert_eq!(mouse_owner(true, false), MouseOwner::Program);
        assert_eq!(mouse_owner(false, false), MouseOwner::Shelbi);
    }

    // ---- mouse encoding + translation ----

    fn identity_placement() -> Placement {
        fit(Size::new(80, 24), Size::new(80, 24))
    }

    #[test]
    fn mouse_silent_when_program_not_reporting() {
        let ev = MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            col: 5,
            row: 5,
            mods: Modifiers::NONE,
        };
        let modes = MouseModes { reporting: false, sgr: true, ..MouseModes::default() };
        assert_eq!(encode_mouse(&ev, modes, &identity_placement()), None);
    }

    #[test]
    fn sgr_left_press_is_one_based() {
        let ev = MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            col: 0,
            row: 0,
            mods: Modifiers::NONE,
        };
        let modes = MouseModes { reporting: true, sgr: true, ..MouseModes::default() };
        assert_eq!(
            encode_mouse(&ev, modes, &identity_placement()).unwrap(),
            b"\x1b[<0;1;1M".to_vec()
        );
    }

    #[test]
    fn sgr_release_uses_lowercase_m() {
        let ev = MouseEvent {
            action: MouseAction::Release,
            button: MouseButton::Left,
            col: 3,
            row: 2,
            mods: Modifiers::NONE,
        };
        let modes = MouseModes { reporting: true, sgr: true, ..MouseModes::default() };
        assert_eq!(
            encode_mouse(&ev, modes, &identity_placement()).unwrap(),
            b"\x1b[<0;4;3m".to_vec()
        );
    }

    #[test]
    fn sgr_wheel_up() {
        let ev = MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::WheelUp,
            col: 10,
            row: 10,
            mods: Modifiers::NONE,
        };
        let modes = MouseModes { reporting: true, sgr: true, ..MouseModes::default() };
        assert_eq!(
            encode_mouse(&ev, modes, &identity_placement()).unwrap(),
            b"\x1b[<64;11;11M".to_vec()
        );
    }

    #[test]
    fn coordinates_are_translated_through_a_letterbox() {
        // Session 80x24 centered in a 100x30 viewer (10 col / 3 row margin). A
        // click at viewer (15, 5) maps to session (5, 2).
        let placement = fit(Size::new(80, 24), Size::new(100, 30));
        let ev = MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            col: 15,
            row: 5,
            mods: Modifiers::NONE,
        };
        let modes = MouseModes { reporting: true, sgr: true, ..MouseModes::default() };
        // session (5,2) -> 1-based (6,3)
        assert_eq!(
            encode_mouse(&ev, modes, &placement).unwrap(),
            b"\x1b[<0;6;3M".to_vec()
        );
    }

    #[test]
    fn clicks_in_the_margin_do_not_encode() {
        let placement = fit(Size::new(80, 24), Size::new(100, 30));
        let ev = MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            col: 2, // left margin
            row: 5,
            mods: Modifiers::NONE,
        };
        let modes = MouseModes { reporting: true, sgr: true, ..MouseModes::default() };
        assert_eq!(encode_mouse(&ev, modes, &placement), None);
    }

    #[test]
    fn legacy_x10_encoding() {
        let ev = MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            col: 0,
            row: 0,
            mods: Modifiers::NONE,
        };
        let modes = MouseModes { reporting: true, sgr: false, ..MouseModes::default() };
        // ESC [ M, button 0+32=32 (space), col 0+33=33 ('!'), row 0+33=33 ('!')
        assert_eq!(
            encode_mouse(&ev, modes, &identity_placement()).unwrap(),
            vec![0x1b, b'[', b'M', 32, 33, 33]
        );
    }

    #[test]
    fn dropped_paths_are_escaped_like_a_terminal_drop() {
        // One path: escaped, then the trailing space a terminal pastes.
        assert_eq!(dropped_paths_text(&["/tmp/shot.png"]), "/tmp/shot.png ");
        assert_eq!(
            dropped_paths_text(&["/Users/me/Screen Shot (1).png"]),
            "/Users/me/Screen\\ Shot\\ \\(1\\).png "
        );
        // Several: space-separated, one trailing space.
        assert_eq!(
            dropped_paths_text(&["/Users/me/Screen Shot (1).png", "/tmp/a'b&c", "/tmp/c.png"]),
            "/Users/me/Screen\\ Shot\\ \\(1\\).png /tmp/a\\'b\\&c /tmp/c.png "
        );
        assert_eq!(dropped_paths_text::<&str>(&[]), "");
    }

    fn mouse(action: MouseAction, button: MouseButton) -> MouseEvent {
        MouseEvent { action, button, col: 4, row: 2, mods: Modifiers::NONE }
    }

    #[test]
    fn drags_are_reported_only_under_button_or_any_event_tracking() {
        let drag = mouse(MouseAction::Drag, MouseButton::Left);
        let click_only = MouseModes { reporting: true, sgr: true, ..MouseModes::default() };
        assert_eq!(encode_mouse(&drag, click_only, &identity_placement()), None);
        let button_event = MouseModes { drag: true, ..click_only };
        // Left (0) + motion bit (32), 1-based coordinates.
        assert_eq!(encode_mouse(&drag, button_event, &identity_placement()).unwrap(), b"\x1b[<32;5;3M");
        // A middle-button drag carries its own button code.
        let middle = mouse(MouseAction::Drag, MouseButton::Middle);
        assert_eq!(encode_mouse(&middle, button_event, &identity_placement()).unwrap(), b"\x1b[<33;5;3M");
    }

    #[test]
    fn motion_without_a_button_needs_any_event_tracking() {
        let moved = mouse(MouseAction::Move, MouseButton::Left);
        let button_event = MouseModes { reporting: true, drag: true, sgr: true, ..MouseModes::default() };
        assert_eq!(encode_mouse(&moved, button_event, &identity_placement()), None);
        let any_event = MouseModes { motion: true, ..button_event };
        // "No button" (3) + motion bit (32) = 35.
        assert_eq!(encode_mouse(&moved, any_event, &identity_placement()).unwrap(), b"\x1b[<35;5;3M");
        let legacy = MouseModes { sgr: false, ..any_event };
        assert_eq!(
            encode_mouse(&moved, legacy, &identity_placement()).unwrap(),
            vec![0x1b, b'[', b'M', 35 + 32, 4 + 33, 2 + 33]
        );
    }
}
