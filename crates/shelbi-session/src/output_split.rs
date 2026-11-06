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
//! sequence). Rest is judged by the transitions in [`crate::vt`], the emulator's
//! own, so an ESC, CAN or SUB ends an open string here exactly when it does
//! there. A pathological stream that never reaches Ground is bounded by
//! [`MAX_HELD`]; past that the buffer is flushed anyway, accepting a torn edge
//! rather than growing without bound.

use crate::vt::State;

/// Hold at most this many un-rested bytes before flushing regardless. Larger
/// than any real escape sequence (OSC titles, DCS) a child emits in one burst.
const MAX_HELD: usize = 1 << 20; // 1 MiB

/// Incremental VT rest-state tracker plus a pending buffer. Feed it raw PTY
/// reads; it yields the complete, rest-aligned prefixes to emit as frames.
#[derive(Default)]
pub struct RestSplitter {
    pending: Vec<u8>,
    /// The parser state after everything fed so far, emitted or still pending.
    state: State,
    /// The last offset in `pending` at which the parser was at rest.
    rest: usize,
}

impl RestSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `data` and return the longest rest-aligned prefix now available to
    /// emit (empty if the whole buffer is mid-sequence and under [`MAX_HELD`]).
    /// The returned bytes are drained from the pending buffer.
    pub fn push(&mut self, data: &[u8]) -> Vec<u8> {
        let start = self.pending.len();
        self.pending.extend_from_slice(data);
        for (i, &b) in data.iter().enumerate() {
            if self.state.at_rest() {
                self.rest = start + i;
            }
            self.state = self.state.step(b);
        }
        if self.state.at_rest() {
            self.rest = self.pending.len();
        }
        let mut cut = self.rest;
        if self.pending.len() - cut >= MAX_HELD {
            // Pathological: never reached Ground. Flush to stay bounded. The
            // state carries on, so the rest of this sequence is still tracked.
            cut = self.pending.len();
        }
        self.rest = 0;
        self.pending.drain(..cut).collect()
    }

    /// Flush any remaining buffered bytes (called at EOF, when no more input can
    /// complete a trailing partial sequence).
    pub fn flush(&mut self) -> Vec<u8> {
        self.rest = 0;
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// True if a fresh parser reading `bytes` finishes at Ground.
    fn ends_at_rest(bytes: &[u8]) -> bool {
        bytes
            .iter()
            .fold(State::Ground, |st, &b| st.step(b))
            .at_rest()
    }

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
                assert!(ends_at_rest(&piece), "emitted a torn chunk");
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

    /// The audit's freeze: nothing later reaches Ground for a parser that lets
    /// an OSC run through an ESC, so output is held until BEL or 1 MiB.
    #[test]
    fn output_passes_through_after_an_esc_interrupted_osc() {
        let mut s = RestSplitter::new();
        assert_eq!(s.push(b"\x1b]0;x\x1b[31mhi"), b"\x1b]0;x\x1b[31mhi");

        let mut s = RestSplitter::new();
        assert_eq!(s.push(b"\x1b]0;x"), b"");
        assert_eq!(s.push(b"\x1b[31m"), b"\x1b]0;x\x1b[31m");
        assert_eq!(s.push(b"hi"), b"hi");
    }

    #[test]
    fn esc_interrupts_every_kind_of_string_and_a_csi() {
        for opener in [
            b"\x1bPqdata".as_slice(),
            b"\x1b_data",
            b"\x1bXdata",
            b"\x1b^data",
            b"\x1b[1;2",
        ] {
            let mut s = RestSplitter::new();
            assert_eq!(s.push(opener), b"", "{opener:?} should be held");
            assert_eq!(s.push(b"\x1b[0mok"), [opener, b"\x1b[0mok"].concat());
        }
    }

    #[test]
    fn can_and_sub_end_a_sequence() {
        for abort in [0x18u8, 0x1a] {
            for opener in [
                b"\x1b]0;x".as_slice(),
                b"\x1bPqx",
                b"\x1b_x",
                b"\x1b[1",
                b"\x1b",
            ] {
                let mut s = RestSplitter::new();
                assert_eq!(s.push(opener), b"", "{opener:?} should be held");
                let want = [opener, &[abort], b"ok"].concat();
                assert_eq!(s.push(&[&[abort], b"ok".as_slice()].concat()), want);
            }
        }
    }

    #[test]
    fn st_split_across_pushes_ends_the_string() {
        let mut s = RestSplitter::new();
        assert_eq!(s.push(b"\x1b]0;x\x1b"), b"");
        assert_eq!(s.push(b"\\z"), b"\x1b]0;x\x1b\\z");
    }

    /// BEL ends an OSC but not a DCS or APC, to the emulator as here. Cutting a
    /// frame there would resume a client mid-string, misreading the rest as text.
    #[test]
    fn bel_does_not_end_a_dcs_or_apc() {
        for opener in [b"\x1bPqx\x07".as_slice(), b"\x1b_x\x07"] {
            let mut s = RestSplitter::new();
            assert_eq!(s.push(opener), b"");
            assert_eq!(s.push(b"\x1b\\"), [opener, b"\x1b\\"].concat());
        }
    }

    #[test]
    fn multi_byte_intermediates_are_held_to_their_final_byte() {
        let mut s = RestSplitter::new();
        assert_eq!(s.push(b"\x1b#"), b"");
        assert_eq!(s.push(b"8ok"), b"\x1b#8ok");
    }

    #[test]
    fn a_string_past_the_limit_flushes_and_stays_tracked() {
        let mut s = RestSplitter::new();
        assert_eq!(s.push(b"ok\x1b]0;"), b"ok");
        let chunk = vec![b'x'; 8192];
        let mut released = 0;
        for _ in 0..(MAX_HELD / chunk.len()) {
            released += s.push(&chunk).len();
            assert!(s.pending.len() <= MAX_HELD, "held {}", s.pending.len());
        }
        assert!(released > 0, "a string past the limit must be flushed");
        // Still inside the string: a resumed read of its tail stays held, and
        // isn't taken for text just because the earlier part went out.
        assert_eq!(s.push(b"more"), b"");
        assert_eq!(s.push(b"\x07z"), b"more\x07z");
    }

    #[test]
    fn the_rest_offset_stays_correct_across_drains() {
        let mut s = RestSplitter::new();
        assert_eq!(s.push(b"ok\x1b]0;"), b"ok");
        // The rest offset moves back to zero after a drain; a later sequence
        // must still cut at the point it reached rest, not at the old offset.
        assert_eq!(s.push(b"\x07ab\x1b["), b"\x1b]0;\x07ab");
        assert_eq!(s.push(b"0m"), b"\x1b[0m");
    }
}
