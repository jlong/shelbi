//! The startup query responder.
//!
//! This is the crux of the whole spike. In the target architecture the
//! `shelbi __session` process spawns an agent into a PTY it owns, with **no
//! rendering client attached yet**. A full-screen agent's first act is to
//! interrogate the terminal: cursor position (DSR 6n), device attributes
//! (DA1/DA2), foreground/background colors (OSC 10/11), and — for agents that
//! use the Kitty keyboard protocol, which Claude Code does for Shift+Enter —
//! the Kitty progressive-enhancement flags (CSI ? u).
//!
//! If nothing answers, Codex gives up and exits ("cursor-position reply late")
//! and Claude Code falls back to a degraded key model. tmux answers these today
//! because the PTY is tmux's; once Shelbi owns the PTY, Shelbi must answer.
//!
//! So this responder is a pure byte-stream scanner: it watches the child's
//! output for query sequences and writes replies back onto the PTY master
//! (i.e. onto the child's stdin). It is deliberately independent of the render
//! emulator — the session process answers queries whether or not a UI is
//! attached. It also tracks the Kitty flag stack the child pushes, because the
//! input encoder needs it to encode Shift+Enter correctly.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Shared, lock-free view of the active Kitty keyboard-protocol flags. The
/// responder updates it as the child pushes/pops/sets; the input encoder reads
/// it to decide whether Shift+Enter is `CSI 13;2u` or a bare CR.
#[derive(Clone, Default)]
pub struct KittyFlags(Arc<AtomicU8>);

impl KittyFlags {
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

/// What the responder decided to do with one scanned query. Returned so callers
/// (and tests) can log exactly which queries an agent asked and how we replied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub query: &'static str,
    pub reply: Vec<u8>,
}

/// Snapshot of where the render emulator thinks the cursor is, 1-based, so a
/// DSR-6 reply reflects reality. The session process reads this off its own
/// headless emulator.
pub trait CursorSource {
    fn cursor_1based(&self) -> (u16, u16);
}

/// A fixed cursor, for headless runs and tests.
pub struct FixedCursor(pub u16, pub u16);
impl CursorSource for FixedCursor {
    fn cursor_1based(&self) -> (u16, u16) {
        (self.0, self.1)
    }
}

pub struct Responder {
    kitty: KittyFlags,
    kitty_stack: Vec<u8>,
    // Carry a partial trailing escape sequence across read boundaries.
    residual: Vec<u8>,
}

impl Responder {
    pub fn new(kitty: KittyFlags) -> Self {
        Self {
            kitty,
            kitty_stack: Vec::new(),
            residual: Vec::new(),
        }
    }

    pub fn flags(&self) -> KittyFlags {
        self.kitty.clone()
    }

    /// Scan one chunk of child output. Returns the replies to write back to the
    /// PTY master, in order. Recognized query bytes are *not* stripped — the
    /// render emulator can still see (and harmlessly ignore) them.
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
        // CSI sequences: ESC [ ...
        if s.len() >= 2 && s[1] == b'[' {
            return self.match_csi(s, cursor);
        }
        // OSC sequences: ESC ] ...
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
            b'n' => {
                // Device Status Report.
                match params {
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
                }
            }
            b'c' => {
                // Device Attributes.
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
            b'q' if params == b">0" || params == b">" => {
                // XTVERSION.
                answer(
                    "XTVERSION",
                    b"\x1bP>|rt-spike-agents(0.0)\x1b\\".to_vec(),
                )
            }
            b'u' => self.match_kitty(params, consumed),
            _ => Match::NotAQuery,
        }
    }

    fn match_kitty(&mut self, params: &[u8], consumed: usize) -> Match {
        // CSI ? u          -> query current flags: reply CSI ? <flags> u
        // CSI > flags u    -> push flags
        // CSI < number u   -> pop <number> entries
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
                if let Some(prev) = self.kitty_stack.pop() {
                    self.kitty.set(prev);
                } else {
                    self.kitty.set(0);
                }
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
                    reply: b"\x1b]10;rgb:cccc/cccc/cccc\x07".to_vec(),
                },
            ),
            _ if body.starts_with(b"11;") && is_query => Match::Answered(
                consumed,
                Answer {
                    query: "OSC 11 background color",
                    reply: b"\x1b]11;rgb:0000/0000/0000\x07".to_vec(),
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
        Responder::new(KittyFlags::default())
    }

    #[test]
    fn answers_cursor_position_report() {
        let mut r = resp();
        let a = r.scan(b"\x1b[6n", &FixedCursor(12, 34));
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].reply, b"\x1b[12;34R");
    }

    #[test]
    fn answers_primary_device_attributes() {
        let mut r = resp();
        let a = r.scan(b"\x1b[c", &FixedCursor(1, 1));
        assert_eq!(a[0].reply, b"\x1b[?62;22c");
        let a = r.scan(b"\x1b[0c", &FixedCursor(1, 1));
        assert_eq!(a[0].reply, b"\x1b[?62;22c");
    }

    #[test]
    fn answers_secondary_device_attributes() {
        let mut r = resp();
        let a = r.scan(b"\x1b[>c", &FixedCursor(1, 1));
        assert_eq!(a[0].reply, b"\x1b[>1;9500;0c");
    }

    #[test]
    fn answers_status_report() {
        let mut r = resp();
        let a = r.scan(b"\x1b[5n", &FixedCursor(1, 1));
        assert_eq!(a[0].reply, b"\x1b[0n");
    }

    #[test]
    fn answers_color_queries_but_not_sets() {
        let mut r = resp();
        let q = r.scan(b"\x1b]11;?\x07", &FixedCursor(1, 1));
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].reply, b"\x1b]11;rgb:0000/0000/0000\x07");
        // A set (no "?") must not be answered.
        let s = r.scan(b"\x1b]11;rgb:1234/1234/1234\x07", &FixedCursor(1, 1));
        assert!(s.is_empty());
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
        // Agent queries support first.
        let a = r.scan(b"\x1b[?u", &FixedCursor(1, 1));
        assert_eq!(a[0].reply, b"\x1b[?0u");
        assert!(!flags.disambiguate_active());
        // Agent pushes "disambiguate escape codes" (bit 0).
        r.scan(b"\x1b[>1u", &FixedCursor(1, 1));
        assert!(flags.disambiguate_active());
        // Pop restores.
        r.scan(b"\x1b[<1u", &FixedCursor(1, 1));
        assert!(!flags.disambiguate_active());
    }

    #[test]
    fn query_split_across_two_reads_is_answered() {
        let mut r = resp();
        let a1 = r.scan(b"ab\x1b[6", &FixedCursor(5, 7));
        assert!(a1.is_empty());
        let a2 = r.scan(b"n", &FixedCursor(5, 7));
        assert_eq!(a2[0].reply, b"\x1b[5;7R");
    }

    #[test]
    fn plain_output_yields_no_answers() {
        let mut r = resp();
        let a = r.scan(b"hello \x1b[1mworld\x1b[0m\r\n", &FixedCursor(1, 1));
        assert!(a.is_empty());
    }
}
