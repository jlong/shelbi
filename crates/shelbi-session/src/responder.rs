//! The startup query responder — the crux of a client-less session.
//!
//! In the remove-tmux architecture the `shelbi __session` process spawns an
//! agent into a PTY it owns with **no rendering client attached yet**. A
//! full-screen agent's first act is to interrogate the terminal: cursor
//! position (DSR 6n), device attributes (DA1/DA2), foreground/background colors
//! (OSC 10/11), and — for agents that use the Kitty keyboard protocol, which
//! Claude Code does for Shift+Enter — the Kitty progressive-enhancement flags
//! (`CSI ? u`).
//!
//! tmux answers these today because the PTY is tmux's. Once Shelbi owns the PTY,
//! Shelbi must answer, from the session process, whether or not a UI is
//! attached: Codex stalls (and on older builds exits) without a cursor-position
//! reply, and Claude Code's Shift+Enter fidelity depends on the Kitty handshake.
//!
//! The responder is a pure byte-stream scanner: it watches the child's output
//! for query sequences and returns the replies to write back onto the PTY master
//! (the child's stdin). It is deliberately independent of the render emulator,
//! reading only a [`CursorSource`] (the live emulator cursor) and a
//! [`ColorState`] (a dark default until a client reports its real colors).
//!
//! Ported from the Phase 0 `rt-spike-agents` responder, which proved it end to
//! end against real Claude Code and Codex.

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::Arc;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Shared, lock-free view of the active Kitty keyboard-protocol flags. The
/// responder updates it as the child pushes/pops/sets; an input encoder reads it
/// to decide whether Shift+Enter is `CSI 13;2u` or a bare CR.
#[derive(Clone, Default)]
pub struct KittyFlags(Arc<AtomicU8>);

impl KittyFlags {
    /// Current flag byte.
    pub fn get(&self) -> u8 {
        self.0.load(Ordering::Relaxed)
    }
    fn set(&self, v: u8) {
        self.0.store(v, Ordering::Relaxed);
    }
    /// bit 0 of the Kitty flags = "disambiguate escape codes", which is what
    /// makes Shift+Enter distinguishable from Enter.
    pub fn disambiguate_active(&self) -> bool {
        self.get() & 0x01 != 0
    }
}

/// Shared default foreground/background the session answers color queries with.
///
/// Before any client has reported its real colors the session uses a **dark
/// default** (light-grey on black). When a client connects and reports colors in
/// its hello, the session updates this and later color queries reflect it.
/// Packed as `0x00RRGGBB` in two atomics so the PTY reader thread can read it
/// without locking.
#[derive(Clone)]
pub struct ColorState {
    fg: Arc<AtomicU32>,
    bg: Arc<AtomicU32>,
}

impl Default for ColorState {
    fn default() -> Self {
        Self {
            // Dark default: light grey foreground on a black background.
            fg: Arc::new(AtomicU32::new(0x00cc_cccc)),
            bg: Arc::new(AtomicU32::new(0x0000_0000)),
        }
    }
}

impl ColorState {
    /// Report the client's real colors (from its hello), overriding the dark
    /// default for subsequent queries.
    pub fn set(&self, fg: (u8, u8, u8), bg: (u8, u8, u8)) {
        self.fg.store(pack(fg), Ordering::Relaxed);
        self.bg.store(pack(bg), Ordering::Relaxed);
    }

    fn foreground(&self) -> (u8, u8, u8) {
        unpack(self.fg.load(Ordering::Relaxed))
    }

    fn background(&self) -> (u8, u8, u8) {
        unpack(self.bg.load(Ordering::Relaxed))
    }
}

fn pack((r, g, b): (u8, u8, u8)) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

fn unpack(v: u32) -> (u8, u8, u8) {
    (((v >> 16) & 0xff) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8)
}

/// An xterm `rgb:RRRR/GGGG/BBBB` string, doubling each 8-bit channel to 16-bit
/// as terminals report.
fn rgb_string(c: (u8, u8, u8)) -> String {
    format!("rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}", c.0, c.1, c.2)
}

/// A query the responder recognized and the bytes it decided to reply with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// Short label for logging / tests.
    pub query: &'static str,
    /// Raw reply bytes to write back onto the PTY master.
    pub reply: Vec<u8>,
}

/// Where the cursor is, 1-based, so a DSR-6 reply reflects reality. The session
/// reads this off its own headless emulator.
pub trait CursorSource {
    /// `(row, col)`, both 1-based.
    fn cursor_1based(&self) -> (u16, u16);
}

/// A fixed cursor, for tests.
pub struct FixedCursor(pub u16, pub u16);
impl CursorSource for FixedCursor {
    fn cursor_1based(&self) -> (u16, u16) {
        (self.0, self.1)
    }
}

/// The scanning state machine. One per session, fed every chunk of child output.
pub struct Responder {
    kitty: KittyFlags,
    colors: ColorState,
    kitty_stack: Vec<u8>,
    // Carry a partial trailing escape sequence across read boundaries.
    residual: Vec<u8>,
}

impl Responder {
    /// Build a responder sharing the given Kitty-flag and color state.
    pub fn new(kitty: KittyFlags, colors: ColorState) -> Self {
        Self {
            kitty,
            colors,
            kitty_stack: Vec::new(),
            residual: Vec::new(),
        }
    }

    /// Handle to the shared Kitty flags (for the input encoder).
    pub fn flags(&self) -> KittyFlags {
        self.kitty.clone()
    }

    /// Scan one chunk of child output. Returns the replies to write back to the
    /// PTY master, in order. Recognized query bytes are *not* stripped — the
    /// render emulator still sees (and harmlessly ignores) them.
    pub fn scan<C: CursorSource>(&mut self, chunk: &[u8], cursor: &C) -> Vec<Answer> {
        let mut buf = std::mem::take(&mut self.residual);
        buf.extend_from_slice(chunk);
        let mut answers = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != ESC {
                i += 1;
                continue;
            }
            match self.match_at(&buf[i..], cursor) {
                Match::Answered(consumed, answer) => {
                    answers.push(answer);
                    i += consumed;
                }
                Match::Consumed(consumed) => i += consumed,
                Match::Incomplete => {
                    // Possibly a query split across reads; stash the tail.
                    self.residual = buf[i..].to_vec();
                    return answers;
                }
                Match::NotAQuery => i += 1,
            }
        }
        answers
    }

    fn match_at<C: CursorSource>(&mut self, s: &[u8], cursor: &C) -> Match {
        if s.len() >= 2 && s[1] == b'[' {
            return self.match_csi(s, cursor);
        }
        if s.len() >= 2 && s[1] == b']' {
            return self.match_osc(s);
        }
        if s.len() < 2 {
            return Match::Incomplete;
        }
        Match::NotAQuery
    }

    fn match_csi<C: CursorSource>(&mut self, s: &[u8], cursor: &C) -> Match {
        // Find the final byte (0x40..=0x7e) that terminates the CSI.
        let mut j = 2;
        while j < s.len() && !(0x40..=0x7e).contains(&s[j]) {
            j += 1;
        }
        if j >= s.len() {
            return Match::Incomplete;
        }
        let params = &s[2..j];
        let final_byte = s[j];
        let consumed = j + 1;
        let answer = |query, reply: Vec<u8>| Match::Answered(consumed, Answer { query, reply });
        match final_byte {
            b'n' => match params {
                b"5" => answer("DSR-5 status", b"\x1b[0n".to_vec()),
                b"6" => {
                    let (r, c) = cursor.cursor_1based();
                    answer("DSR-6 cursor position", format!("\x1b[{r};{c}R").into_bytes())
                }
                b"?6" => {
                    let (r, c) = cursor.cursor_1based();
                    answer(
                        "DECXCPR extended cursor",
                        format!("\x1b[?{r};{c};1R").into_bytes(),
                    )
                }
                _ => Match::Consumed(consumed),
            },
            b'c' => {
                if params.is_empty() || params == b"0" {
                    // Primary DA: claim a VT220 with sixel-ish extensions.
                    answer("DA1 primary attributes", b"\x1b[?62;22c".to_vec())
                } else if params == b">" || params == b">0" {
                    // Secondary DA: report a terminal version.
                    answer("DA2 secondary attributes", b"\x1b[>1;9500;0c".to_vec())
                } else {
                    Match::Consumed(consumed)
                }
            }
            b'q' if params == b">0" || params == b">" => answer(
                "XTVERSION",
                b"\x1bP>|shelbi\x1b\\".to_vec(),
            ),
            b'u' => self.match_kitty(params, consumed),
            _ => Match::NotAQuery,
        }
    }

    fn match_kitty(&mut self, params: &[u8], consumed: usize) -> Match {
        // CSI ? u              -> query current flags: reply CSI ? <flags> u
        // CSI > flags u        -> push flags
        // CSI < number u       -> pop <number> entries
        // CSI = flags ; mode u -> set flags (mode 1=set,2=add,3=remove)
        if params == b"?" {
            let reply = format!("\x1b[?{}u", self.kitty.get());
            return Match::Answered(
                consumed,
                Answer {
                    query: "Kitty keyboard flags query",
                    reply: reply.into_bytes(),
                },
            );
        }
        if let Some(rest) = params.strip_prefix(b">") {
            let flags = parse_u8(rest).unwrap_or(0);
            self.kitty_stack.push(self.kitty.get());
            self.kitty.set(flags);
            return Match::Consumed(consumed);
        }
        if let Some(rest) = params.strip_prefix(b"<") {
            let n = parse_u8(rest).unwrap_or(1).max(1);
            for _ in 0..n {
                let prev = self.kitty_stack.pop().unwrap_or(0);
                self.kitty.set(prev);
            }
            return Match::Consumed(consumed);
        }
        if let Some(rest) = params.strip_prefix(b"=") {
            let mut it = rest.split(|&b| b == b';');
            let flags = it.next().and_then(parse_u8).unwrap_or(0);
            let mode = it.next().and_then(parse_u8).unwrap_or(1);
            let cur = self.kitty.get();
            let next = match mode {
                2 => cur | flags,
                3 => cur & !flags,
                _ => flags,
            };
            self.kitty.set(next);
            return Match::Consumed(consumed);
        }
        Match::NotAQuery
    }

    fn match_osc(&mut self, s: &[u8]) -> Match {
        // OSC ... terminated by BEL or ESC \ (ST).
        let mut j = 2;
        let mut term_len = 0;
        while j < s.len() {
            if s[j] == BEL {
                term_len = 1;
                break;
            }
            if s[j] == ESC && j + 1 < s.len() && s[j + 1] == b'\\' {
                term_len = 2;
                break;
            }
            if s[j] == ESC {
                // ESC not yet followed by its pair; wait for more.
                return Match::Incomplete;
            }
            j += 1;
        }
        if term_len == 0 {
            return Match::Incomplete;
        }
        let body = &s[2..j];
        let consumed = j + term_len;
        // Only answer the "?" color *queries*; ignore sets.
        let is_query = body.ends_with(b";?");
        match () {
            _ if body.starts_with(b"10;") && is_query => Match::Answered(
                consumed,
                Answer {
                    query: "OSC 10 foreground color",
                    reply: format!("\x1b]10;{}\x07", rgb_string(self.colors.foreground()))
                        .into_bytes(),
                },
            ),
            _ if body.starts_with(b"11;") && is_query => Match::Answered(
                consumed,
                Answer {
                    query: "OSC 11 background color",
                    reply: format!("\x1b]11;{}\x07", rgb_string(self.colors.background()))
                        .into_bytes(),
                },
            ),
            _ => Match::Consumed(consumed),
        }
    }
}

enum Match {
    /// A query we recognized and answered. (bytes consumed, the answer)
    Answered(usize, Answer),
    /// A sequence we recognized but do not answer. (bytes consumed)
    Consumed(usize),
    /// The sequence runs past the end of the buffer; wait for more bytes.
    Incomplete,
    /// Not a sequence we care about; advance one byte.
    NotAQuery,
}

fn parse_u8(bytes: &[u8]) -> Option<u8> {
    if bytes.is_empty() {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp() -> Responder {
        Responder::new(KittyFlags::default(), ColorState::default())
    }

    #[test]
    fn answers_cursor_position_report() {
        let mut r = resp();
        let a = r.scan(b"\x1b[6n", &FixedCursor(12, 34));
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].reply, b"\x1b[12;34R");
    }

    #[test]
    fn answers_primary_and_secondary_device_attributes() {
        let mut r = resp();
        assert_eq!(r.scan(b"\x1b[c", &FixedCursor(1, 1))[0].reply, b"\x1b[?62;22c");
        assert_eq!(r.scan(b"\x1b[0c", &FixedCursor(1, 1))[0].reply, b"\x1b[?62;22c");
        assert_eq!(
            r.scan(b"\x1b[>c", &FixedCursor(1, 1))[0].reply,
            b"\x1b[>1;9500;0c"
        );
    }

    #[test]
    fn answers_status_report() {
        let mut r = resp();
        assert_eq!(r.scan(b"\x1b[5n", &FixedCursor(1, 1))[0].reply, b"\x1b[0n");
    }

    #[test]
    fn color_query_uses_dark_default_then_client_colors() {
        let colors = ColorState::default();
        let mut r = Responder::new(KittyFlags::default(), colors.clone());
        // Dark default background is black.
        let bg = r.scan(b"\x1b]11;?\x07", &FixedCursor(1, 1));
        assert_eq!(bg[0].reply, b"\x1b]11;rgb:0000/0000/0000\x07");
        // Client reports a light background; later queries reflect it.
        colors.set((0, 0, 0), (0xff, 0xff, 0xff));
        let bg2 = r.scan(b"\x1b]11;?\x07", &FixedCursor(1, 1));
        assert_eq!(bg2[0].reply, b"\x1b]11;rgb:ffff/ffff/ffff\x07");
        // A set (no "?") is never answered.
        assert!(r
            .scan(b"\x1b]11;rgb:1234/1234/1234\x07", &FixedCursor(1, 1))
            .is_empty());
    }

    #[test]
    fn answers_osc_with_st_terminator() {
        let mut r = resp();
        let a = r.scan(b"\x1b]10;?\x1b\\", &FixedCursor(1, 1));
        assert_eq!(a[0].query, "OSC 10 foreground color");
    }

    #[test]
    fn tracks_kitty_push_pop_and_answers_query() {
        let mut r = resp();
        let flags = r.flags();
        assert_eq!(r.scan(b"\x1b[?u", &FixedCursor(1, 1))[0].reply, b"\x1b[?0u");
        assert!(!flags.disambiguate_active());
        r.scan(b"\x1b[>1u", &FixedCursor(1, 1));
        assert!(flags.disambiguate_active());
        r.scan(b"\x1b[<1u", &FixedCursor(1, 1));
        assert!(!flags.disambiguate_active());
    }

    #[test]
    fn query_split_across_two_reads_is_answered() {
        let mut r = resp();
        assert!(r.scan(b"ab\x1b[6", &FixedCursor(5, 7)).is_empty());
        assert_eq!(r.scan(b"n", &FixedCursor(5, 7))[0].reply, b"\x1b[5;7R");
    }

    #[test]
    fn plain_output_yields_no_answers() {
        let mut r = resp();
        assert!(r
            .scan(b"hello \x1b[1mworld\x1b[0m\r\n", &FixedCursor(1, 1))
            .is_empty());
    }
}
