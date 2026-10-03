//! Input encoding: crossterm UI events -> bytes written to the PTY master.
//!
//! The session process never passes raw terminal input through — a client may
//! be on any terminal, or none. It decodes UI events and re-encodes them for
//! the agent, honoring the modes the agent asked for (Kitty keyboard flags,
//! mouse protocol, bracketed paste). These functions are the encoder; the
//! interesting case is Shift+Enter, which only survives when the Kitty
//! "disambiguate escape codes" flag is active (Claude Code pushes it).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// Encode a key event to the bytes an agent expects on its stdin.
///
/// `kitty_disambiguate` is the live flag the responder tracks: when the agent
/// has pushed Kitty progressive enhancement, modified keys (Shift+Enter,
/// Ctrl+Enter, etc.) are encoded as `CSI <code> ; <mods> u`. Without it we fall
/// back to the legacy encoding, where Shift+Enter is indistinguishable from
/// Enter — exactly the fidelity gap the plan calls out.
pub fn encode_key(ev: &KeyEvent, kitty_disambiguate: bool) -> Vec<u8> {
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);

    // Kitty modifier parameter: 1 + bitmask(shift=1, alt=2, ctrl=4).
    let kitty_mods = 1 + (shift as u8) + 2 * (alt as u8) + 4 * (ctrl as u8);
    let has_mods = shift || ctrl || alt;

    match ev.code {
        KeyCode::Enter => {
            if kitty_disambiguate && has_mods {
                // 13 = Enter functional key code in the Kitty protocol.
                format!("\x1b[13;{kitty_mods}u").into_bytes()
            } else {
                // Legacy: CR. Shift/Ctrl are lost here — the documented gap.
                b"\r".to_vec()
            }
        }
        KeyCode::Tab => {
            if shift {
                b"\x1b[Z".to_vec() // back-tab (CSI Z), works without Kitty
            } else {
                b"\t".to_vec()
            }
        }
        KeyCode::Backspace => b"\x7f".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Up => arrow(b'A', kitty_mods, has_mods),
        KeyCode::Down => arrow(b'B', kitty_mods, has_mods),
        KeyCode::Right => arrow(b'C', kitty_mods, has_mods),
        KeyCode::Left => arrow(b'D', kitty_mods, has_mods),
        KeyCode::Char(c) => encode_char(c, ctrl, alt),
        _ => Vec::new(),
    }
}

fn arrow(final_byte: u8, mods: u8, has_mods: bool) -> Vec<u8> {
    if has_mods {
        format!("\x1b[1;{mods}{}", final_byte as char).into_bytes()
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

fn encode_char(c: char, ctrl: bool, alt: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }
    if ctrl {
        // Control maps a..z and a few symbols to 0x01..0x1f.
        if c.is_ascii_alphabetic() {
            out.push((c.to_ascii_uppercase() as u8) & 0x1f);
            return out;
        }
        match c {
            ' ' => out.push(0),
            '@' => out.push(0),
            _ => {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
        }
        return out;
    }
    let mut b = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
    out
}

/// SGR-encoded (1006) mouse report. Shelbi only forwards the mouse to an agent
/// that asked for it; this is the wire format for that forward. Coordinates are
/// translated to the agent's 1-based cell grid before calling.
pub fn encode_mouse_sgr(ev: &MouseEvent, col_1based: u16, row_1based: u16) -> Vec<u8> {
    let (cb, release) = match ev.kind {
        MouseEventKind::Down(b) => (button_code(b), false),
        MouseEventKind::Up(b) => (button_code(b), true),
        MouseEventKind::Drag(b) => (button_code(b) + 32, false),
        MouseEventKind::Moved => (35, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
    };
    let modifiers = mouse_mods(ev.modifiers);
    let final_byte = if release { 'm' } else { 'M' };
    format!("\x1b[<{};{};{}{}", cb + modifiers, col_1based, row_1based, final_byte).into_bytes()
}

fn button_code(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn mouse_mods(m: KeyModifiers) -> u16 {
    let mut v = 0;
    if m.contains(KeyModifiers::SHIFT) {
        v += 4;
    }
    if m.contains(KeyModifiers::ALT) {
        v += 8;
    }
    if m.contains(KeyModifiers::CONTROL) {
        v += 16;
    }
    v
}

/// Wrap pasted text in bracketed-paste markers. Called only when the agent has
/// enabled bracketed paste (DECSET 2004); otherwise the raw text is sent.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.as_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn plain_enter_is_cr() {
        assert_eq!(encode_key(&key(KeyCode::Enter, KeyModifiers::NONE), true), b"\r");
    }

    #[test]
    fn shift_enter_needs_kitty() {
        let k = key(KeyCode::Enter, KeyModifiers::SHIFT);
        // Without Kitty: lost, collapses to CR (the gap).
        assert_eq!(encode_key(&k, false), b"\r");
        // With Kitty disambiguate: distinct CSI 13;2u.
        assert_eq!(encode_key(&k, true), b"\x1b[13;2u");
    }

    #[test]
    fn ctrl_enter_under_kitty() {
        let k = key(KeyCode::Enter, KeyModifiers::CONTROL);
        assert_eq!(encode_key(&k, true), b"\x1b[13;5u");
    }

    #[test]
    fn ctrl_c_is_etx() {
        assert_eq!(encode_key(&key(KeyCode::Char('c'), KeyModifiers::CONTROL), false), vec![0x03]);
    }

    #[test]
    fn alt_char_prefixes_esc() {
        assert_eq!(encode_key(&key(KeyCode::Char('x'), KeyModifiers::ALT), false), vec![0x1b, b'x']);
    }

    #[test]
    fn shift_tab_is_back_tab() {
        assert_eq!(encode_key(&key(KeyCode::Tab, KeyModifiers::SHIFT), false), b"\x1b[Z");
    }

    #[test]
    fn modified_arrow_uses_csi_1_mods() {
        assert_eq!(encode_key(&key(KeyCode::Up, KeyModifiers::CONTROL), false), b"\x1b[1;5A");
        assert_eq!(encode_key(&key(KeyCode::Left, KeyModifiers::NONE), false), b"\x1b[D");
    }

    #[test]
    fn mouse_left_down_sgr() {
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(encode_mouse_sgr(&ev, 10, 5), b"\x1b[<0;10;5M");
    }

    #[test]
    fn mouse_wheel_up_sgr() {
        let ev = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(encode_mouse_sgr(&ev, 3, 4), b"\x1b[<64;3;4M");
    }

    #[test]
    fn mouse_release_uses_lowercase_m() {
        let ev = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(encode_mouse_sgr(&ev, 2, 2), b"\x1b[<0;2;2m");
    }

    #[test]
    fn bracketed_paste_wraps() {
        assert_eq!(encode_paste("hi", true), b"\x1b[200~hi\x1b[201~");
        assert_eq!(encode_paste("hi", false), b"hi");
    }
}
