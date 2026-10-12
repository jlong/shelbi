//! Rest-boundary splitting for the live output stream.
//!
//! Output is broadcast as sequence-numbered frames, and a client that attaches
//! (or is dropped to a fresh replay) resumes at a frame edge *after* replay has
//! reset its parser to Ground. So a frame edge must never fall inside an escape
//! sequence or a multi-byte UTF-8 character: if it did, the replay — which
//! reflects the session emulator's fully-parsed state — would omit the partial
//! trailing sequence, and the resuming client would misparse the bytes that
//! straddle the edge as text (the Phase 0 spike case (c);
//! `docs/removing-tmux/phase0/emulator-replay.md`).
//!
//! [`RestSplitter`] buffers whatever trails the last at-rest offset and emits
//! only complete, Ground-terminated prefixes. Because every emitted frame ends
//! at Ground, the session emulator (fed the same frames) is always at a
//! complete-token boundary that matches a frame edge, and the replay/live split
//! is always clean. A single streaming parser could buffer partial sequences on
//! its own — this matters specifically for the *frame edges* reconnect resumes
//! at.
//!
//! One edge case: a program that writes an incomplete sequence and then pauses
//! would have its bytes held until it completes (correct — you must not split a
//! sequence). A pathological stream that never reaches Ground is bounded by
//! [`MAX_HELD`]; past that the buffer is flushed anyway, accepting a torn edge
//! rather than growing without bound.

/// Hold at most this many un-rested bytes before flushing regardless. Larger
/// than any real escape sequence (OSC titles, DCS) a child emits in one burst.
const MAX_HELD: usize = 1 << 20; // 1 MiB

/// Incremental VT rest-state tracker plus a pending buffer. Feed it raw PTY
/// reads; it yields the complete, rest-aligned prefixes to emit as frames.
#[derive(Default)]
pub struct RestSplitter {
    pending: Vec<u8>,
}

impl RestSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `data` and return the longest rest-aligned prefix now available to
    /// emit (empty if the whole buffer is mid-sequence and under [`MAX_HELD`]).
    /// The returned bytes are drained from the pending buffer.
    pub fn push(&mut self, data: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(data);
        let mut cut = rest_cut(&self.pending);
        if cut == 0 && self.pending.len() >= MAX_HELD {
            // Pathological: never reached Ground. Flush to stay bounded.
            cut = self.pending.len();
        }
        self.pending.drain(..cut).collect()
    }

    /// Flush any remaining buffered bytes (called at EOF, when no more input can
    /// complete a trailing partial sequence).
    pub fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

/// Parser rest states: enough of a VT state machine to know when the parser sits
/// at a token boundary (Ground, no pending UTF-8 continuation).
#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    Ground,
    Esc,
    Csi,
    /// OSC/DCS-style string; `true` once an ESC was seen (awaiting `\` for ST).
    Str(bool),
    Charset,
    /// UTF-8 continuation bytes still expected.
    Utf8(u8),
}

fn step(st: St, b: u8) -> St {
    match st {
        St::Ground => match b {
            0x1b => St::Esc,
            0xc2..=0xdf => St::Utf8(1),
            0xe0..=0xef => St::Utf8(2),
            0xf0..=0xf4 => St::Utf8(3),
            _ => St::Ground,
        },
        St::Utf8(n) => {
            if (0x80..=0xbf).contains(&b) {
                if n == 1 {
                    St::Ground
                } else {
                    St::Utf8(n - 1)
                }
            } else {
                // Malformed; re-interpret this byte from Ground.
                step(St::Ground, b)
            }
        }
        St::Esc => match b {
            b'[' => St::Csi,
            b']' | b'P' | b'X' | b'^' | b'_' => St::Str(false),
            b'(' | b')' | b'*' | b'+' => St::Charset,
            _ => St::Ground, // two-byte escape (ESC 7, ESC c, ...)
        },
        St::Csi => match b {
            0x40..=0x7e => St::Ground, // final byte
            _ => St::Csi,              // params / intermediates
        },
        St::Str(seen_esc) => match b {
            0x07 => St::Ground,              // BEL terminates
            b'\\' if seen_esc => St::Ground, // ST (ESC \)
            0x1b => St::Str(true),
            _ => St::Str(false),
        },
        St::Charset => St::Ground,
    }
}

/// The largest offset `<= bytes.len()` at which the parser is at rest (Ground,
/// before that byte). Walking from Ground, this is the last position before a
/// byte where the state is Ground, or `bytes.len()` when the whole buffer ends
/// at Ground. Complete tokens before a trailing partial sequence are included.
fn rest_cut(bytes: &[u8]) -> usize {
    let mut st = St::Ground;
    let mut last = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if st == St::Ground {
            last = i;
        }
        st = step(st, b);
    }
    if st == St::Ground {
        bytes.len()
    } else {
        last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_rested_stream_passes_through() {
        let mut s = RestSplitter::new();
        assert_eq!(s.push(b"hello\r\n"), b"hello\r\n");
        assert!(s.flush().is_empty());
    }

    #[test]
    fn holds_a_partial_sequence_until_it_completes() {
        let mut s = RestSplitter::new();
        // "ABC" is complete; "\x1b[31" is an unfinished CSI, held back.
        assert_eq!(s.push(b"ABC\x1b[31"), b"ABC");
        // The final byte completes it; now the whole red run is emittable.
        assert_eq!(s.push(b"mZ"), b"\x1b[31mZ");
    }

    #[test]
    fn holds_a_partial_utf8_codepoint() {
        let mut s = RestSplitter::new();
        let euro = "€".as_bytes(); // 3 bytes: e2 82 ac
        assert_eq!(s.push(&euro[..1]), b""); // only the lead byte so far
        assert_eq!(s.push(&euro[1..]), euro); // completed
    }

    #[test]
    fn never_splits_mid_sequence_across_many_small_pushes() {
        let stream: &[u8] = b"x\x1b[1;2;3mY\x1b]0;title\x07Z\xf0\x9f\x98\x80end";
        // Feed one byte at a time; concatenated output must equal the input and
        // every emitted chunk must itself end at Ground.
        let mut s = RestSplitter::new();
        let mut out = Vec::new();
        for &b in stream {
            let piece = s.push(&[b]);
            if !piece.is_empty() {
                assert_eq!(rest_cut(&piece), piece.len(), "emitted a torn chunk");
            }
            out.extend_from_slice(&piece);
        }
        out.extend_from_slice(&s.flush());
        assert_eq!(out, stream);
    }

    #[test]
    fn flush_releases_a_dangling_partial() {
        let mut s = RestSplitter::new();
        assert_eq!(s.push(b"ok\x1b["), b"ok");
        assert_eq!(s.flush(), b"\x1b[");
    }
}
