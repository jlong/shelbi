//! Input encoding: keys, mouse, paste, and focus into PTY bytes.
//!
//! This turns a UI's input events into the raw bytes an
//! [`Input`](shelbi_proto::Input) frame carries (or the text a
//! [`Paste`](shelbi_proto::Paste) frame carries). The public types here are
//! toolkit-neutral: a crossterm TUI and a gpui app each map their native
//! events onto [`Key`], [`Modifiers`], and [`MouseEvent`], so no crossterm or
//! gpui type crosses this boundary.
//!
//! Key encoding is delegated to termwiz's encoder (the fiddly
//! application-cursor-key, modify-other-keys, and CSI-u tables), wrapped here
//! so termwiz types stay internal.
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

use termwiz::input::{KeyCode, KeyCodeEncodeModes, KeyboardEncoding, Modifiers as TwMods};

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

    fn to_termwiz(self) -> TwMods {
        let mut m = TwMods::NONE;
        if self.shift {
            m |= TwMods::SHIFT;
        }
        if self.alt {
            m |= TwMods::ALT;
        }
        if self.ctrl {
            m |= TwMods::CTRL;
        }
        if self.sup {
            m |= TwMods::SUPER;
        }
        m
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

impl Key {
    fn to_termwiz(self) -> KeyCode {
        match self {
            Key::Char(c) => KeyCode::Char(c),
            Key::Enter => KeyCode::Enter,
            Key::Tab => KeyCode::Tab,
            Key::Backspace => KeyCode::Backspace,
            Key::Escape => KeyCode::Escape,
            Key::Delete => KeyCode::Delete,
            Key::Insert => KeyCode::Insert,
            Key::Up => KeyCode::UpArrow,
            Key::Down => KeyCode::DownArrow,
            Key::Left => KeyCode::LeftArrow,
            Key::Right => KeyCode::RightArrow,
            Key::Home => KeyCode::Home,
            Key::End => KeyCode::End,
            Key::PageUp => KeyCode::PageUp,
            Key::PageDown => KeyCode::PageDown,
            Key::Function(n) => KeyCode::Function(n),
        }
    }
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

impl KeyEncoding {
    fn modes(self) -> KeyCodeEncodeModes {
        KeyCodeEncodeModes {
            encoding: if self.kitty { KeyboardEncoding::CsiU } else { KeyboardEncoding::Xterm },
            application_cursor_keys: self.application_cursor_keys,
            newline_mode: self.newline_mode,
            modify_other_keys: None,
        }
    }
}

/// Encode a key press into the bytes to write to the PTY.
///
/// Returns an empty vector for a key that has no encoding in the current mode
/// (e.g. a lone modifier, which callers should not pass here).
pub fn encode_key(key: Key, mods: Modifiers, encoding: KeyEncoding) -> Vec<u8> {
    // Under the kitty keyboard protocol, the unmodified Tab *key* is the CSI-u
    // event `ESC[9u` (9 = HT's codepoint). termwiz encodes plain Tab as a bare
    // `\t` regardless of the protocol (its Tab arm never consults the encoding),
    // and a program that enabled the protocol — Claude Code does, pushing
    // `CSI > 5 u` — reads that bare `\t` as literal tab *text*, not a Tab key
    // press, so completion (file paths, slash commands) never fires. Emitting
    // the CSI-u form makes the Tab key register. Modified Tab is left to
    // termwiz: Shift+Tab stays the legacy backtab `ESC[Z`, which Claude Code
    // maps to its Shift+Tab mode cycle (the CSI-u `ESC[9;2u` does not drive it).
    if encoding.kitty && key == Key::Tab && mods == Modifiers::NONE {
        return b"\x1b[9u".to_vec();
    }
    key.to_termwiz()
        .encode(mods.to_termwiz(), encoding.modes(), /* is_down */ true)
        .unwrap_or_default()
        .into_bytes()
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
    /// SGR extended encoding (`ESC[?1006h`). When off, legacy X10 is used.
    pub sgr: bool,
}

/// Encode a mouse event for the program, translating viewer coordinates to the
/// session pane via `placement`.
///
/// Returns `None` when the program is not reporting the mouse, or when the
/// event lands in letterbox margin / outside a clipped window (there is no
/// session cell under it). The ownership decision ([`mouse_owner`]) is the
/// caller's; this only encodes events already destined for the program.
pub fn encode_mouse(ev: &MouseEvent, modes: MouseModes, placement: &Placement) -> Option<Vec<u8>> {
    if !modes.reporting {
        return None;
    }
    let (scol, srow) = placement.viewer_to_session(ev.col, ev.row)?;

    let mut cb = button_code(ev.button);
    if matches!(ev.action, MouseAction::Drag) {
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
        let modes = MouseModes { reporting: false, sgr: true };
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
        let modes = MouseModes { reporting: true, sgr: true };
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
        let modes = MouseModes { reporting: true, sgr: true };
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
        let modes = MouseModes { reporting: true, sgr: true };
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
        let modes = MouseModes { reporting: true, sgr: true };
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
        let modes = MouseModes { reporting: true, sgr: true };
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
        let modes = MouseModes { reporting: true, sgr: false };
        // ESC [ M, button 0+32=32 (space), col 0+33=33 ('!'), row 0+33=33 ('!')
        assert_eq!(
            encode_mouse(&ev, modes, &identity_placement()).unwrap(),
            vec![0x1b, b'[', b'M', 32, 33, 33]
        );
    }
}

