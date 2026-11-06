//! The one VT tokenizer the session's byte-level consumers share.
//!
//! The session reads the child's output with three parsers: the emulator
//! (vte, via `alacritty_terminal`), the [`RestSplitter`] that picks frame edges,
//! and the [`Responder`] that answers terminal queries. When those disagree
//! about where a sequence ends, the failure is silent and sticky: a splitter
//! that thinks a string is still open freezes the live screen, and a responder
//! that thinks so stops answering, hanging a program waiting on its reply.
//!
//! So the splitter and the responder both walk [`State::step`], and `step`
//! follows vte's transition table byte for byte. vte can't serve directly: it
//! doesn't expose its state, and the splitter needs to know when it's at rest.
//! `state_agrees_with_vte_on_random_streams` holds the two together.
//!
//! The rule that matters most is that CAN and SUB return to Ground from any
//! state and ESC starts a new escape from any state, so no sequence can swallow
//! the bytes after it unless the program stops sending escapes altogether.
//!
//! [`RestSplitter`]: crate::output_split::RestSplitter
//! [`Responder`]: crate::responder::Responder

const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const ESC: u8 = 0x1b;
/// C1 string terminator, honored only inside a DCS body (as vte does).
const C1_ST: u8 = 0x9c;

/// The most bytes of one sequence [`Tokenizer`] retains. Far longer than any
/// sequence the responder answers; a longer one is tracked to its end and then
/// dropped, so an unterminated string can't grow memory or get re-scanned.
pub(crate) const MAX_RESIDUAL: usize = 4096;

/// Where the parser is between two bytes of output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum State {
    #[default]
    Ground,
    /// Inside a UTF-8 character: `remaining` bytes to come, the next of which
    /// must fall in `lo..=hi` (narrower than continuation range right after
    /// some lead bytes, which is what rules out overlongs and surrogates).
    Utf8 {
        remaining: u8,
        lo: u8,
        hi: u8,
    },
    Esc,
    /// `ESC` plus intermediates (charset designations, `ESC # 8`, ...).
    EscIntermediate,
    /// Any part of a CSI. vte's entry/param/intermediate/ignore states all leave
    /// on the same bytes, so one state is enough to find where a CSI ends.
    Csi,
    Osc,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsIgnore,
    DcsPassthrough,
    /// SOS, PM or APC. Unlike OSC, BEL does not end these or DCS.
    OpaqueString,
}

impl State {
    /// True at a token boundary: nothing buffered, no sequence or character open.
    pub(crate) fn at_rest(self) -> bool {
        self == State::Ground
    }

    /// The state after `b`.
    pub(crate) fn step(self, b: u8) -> State {
        use State::*;
        match b {
            CAN | SUB => return Ground,
            ESC => return Esc,
            _ => {}
        }
        match self {
            Ground => ground(b),
            Utf8 { remaining, lo, hi } => {
                if !(lo..=hi).contains(&b) {
                    // Malformed; re-interpret this byte from Ground.
                    ground(b)
                } else if remaining == 1 {
                    Ground
                } else {
                    Utf8 {
                        remaining: remaining - 1,
                        lo: 0x80,
                        hi: 0xbf,
                    }
                }
            }
            Esc => match b {
                0x20..=0x2f => EscIntermediate,
                b'P' => DcsEntry,
                b'X' | b'^' | b'_' => OpaqueString,
                b'[' => Csi,
                b']' => Osc,
                0x30..=0x7e => Ground, // two-byte escape (ESC 7, ESC \, ...)
                _ => Esc,
            },
            EscIntermediate => match b {
                0x30..=0x7e => Ground,
                _ => EscIntermediate,
            },
            Csi => match b {
                0x40..=0x7e => Ground,
                _ => Csi,
            },
            Osc => match b {
                BEL => Ground,
                _ => Osc,
            },
            DcsEntry => match b {
                0x20..=0x2f => DcsIntermediate,
                0x30..=0x3f => DcsParam,
                0x40..=0x7e => DcsPassthrough,
                _ => DcsEntry,
            },
            DcsParam => match b {
                0x20..=0x2f => DcsIntermediate,
                0x3c..=0x3f => DcsIgnore,
                0x40..=0x7e => DcsPassthrough,
                _ => DcsParam,
            },
            DcsIntermediate => match b {
                0x30..=0x3f => DcsIgnore,
                0x40..=0x7e => DcsPassthrough,
                _ => DcsIntermediate,
            },
            DcsIgnore => DcsIgnore,
            DcsPassthrough => match b {
                C1_ST => Ground,
                _ => DcsPassthrough,
            },
            OpaqueString => OpaqueString,
        }
    }
}

/// The state after `b` at Ground: only a UTF-8 lead byte leaves it (ESC, the
/// other way out, is handled before this is reached).
fn ground(b: u8) -> State {
    let utf8 = |remaining, lo, hi| State::Utf8 { remaining, lo, hi };
    match b {
        0xc2..=0xdf => utf8(1, 0x80, 0xbf),
        0xe0 => utf8(2, 0xa0, 0xbf),
        0xed => utf8(2, 0x80, 0x9f),
        0xe1..=0xef => utf8(2, 0x80, 0xbf),
        0xf0 => utf8(3, 0x90, 0xbf),
        0xf1..=0xf3 => utf8(3, 0x80, 0xbf),
        0xf4 => utf8(3, 0x80, 0x8f),
        _ => State::Ground,
    }
}

/// A complete sequence whose bytes a consumer may want to read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Token<'a> {
    /// `CSI params final`; `params` is everything between `[` and the final
    /// byte, private markers included (`?6`, `>1`).
    Csi { params: &'a [u8], final_byte: u8 },
    /// An OSC ended by BEL or ST. One aborted by anything else yields no token.
    Osc { body: &'a [u8] },
}

/// [`State`] plus the bytes of the CSI or OSC in progress, for a consumer that
/// reads sequences rather than just their boundaries. Fed one byte at a time, so
/// a sequence split across reads needs no re-scan.
#[derive(Default)]
pub(crate) struct Tokenizer {
    state: State,
    /// The sequence in progress, never longer than [`MAX_RESIDUAL`].
    residual: Vec<u8>,
    overflowed: bool,
    /// The previous byte was an ESC that ended an OSC: `\` completes it as ST.
    osc_awaiting_st: bool,
}

impl Tokenizer {
    /// Consume one byte, returning the sequence it completes, if any.
    pub(crate) fn advance(&mut self, b: u8) -> Option<Token<'_>> {
        let prev = self.state;
        self.state = prev.step(b);
        let osc_awaiting_st = std::mem::take(&mut self.osc_awaiting_st);
        match (prev, b) {
            (State::Esc, b'[' | b']') => {
                self.residual.clear();
                self.overflowed = false;
            }
            (State::Esc, b'\\') if osc_awaiting_st => return self.finish_osc(),
            (State::Csi, 0x20..=0x3f) => self.retain(b),
            (State::Csi, 0x40..=0x7e) => {
                return (!self.overflowed).then_some(Token::Csi {
                    params: &self.residual,
                    final_byte: b,
                });
            }
            (State::Osc, BEL) => return self.finish_osc(),
            (State::Osc, ESC) => self.osc_awaiting_st = true,
            (State::Osc, 0x20..) => self.retain(b),
            _ => {}
        }
        None
    }

    fn retain(&mut self, b: u8) {
        if self.residual.len() < MAX_RESIDUAL {
            self.residual.push(b);
        } else {
            self.overflowed = true;
        }
    }

    fn finish_osc(&self) -> Option<Token<'_>> {
        (!self.overflowed).then_some(Token::Osc {
            body: &self.residual,
        })
    }

    #[cfg(test)]
    pub(crate) fn residual_len(&self) -> usize {
        self.residual.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::{Parser, Perform};

    fn walk(bytes: &[u8]) -> State {
        bytes.iter().fold(State::Ground, |st, &b| st.step(b))
    }

    /// Every token `bytes` yields, flattened to `(kind, body, final byte)`.
    fn tokens(bytes: &[u8]) -> Vec<(&'static str, Vec<u8>, u8)> {
        let mut t = Tokenizer::default();
        let mut out = Vec::new();
        for &b in bytes {
            match t.advance(b) {
                Some(Token::Csi { params, final_byte }) => {
                    out.push(("csi", params.to_vec(), final_byte))
                }
                Some(Token::Osc { body }) => out.push(("osc", body.to_vec(), 0)),
                None => {}
            }
        }
        out
    }

    #[test]
    fn esc_can_and_sub_leave_every_state() {
        let openers: &[&[u8]] = &[
            b"\xe2\x82",
            b"\x1b",
            b"\x1b(",
            b"\x1b[1;",
            b"\x1b]0;x",
            b"\x1bP",
            b"\x1bP1",
            b"\x1bP$",
            b"\x1bP1;<",
            b"\x1bPq",
            b"\x1b_x",
            b"\x1bXx",
            b"\x1b^x",
        ];
        for opener in openers {
            assert_ne!(walk(opener), State::Ground, "{opener:?} should be open");
            for (end, want) in [
                (CAN, State::Ground),
                (SUB, State::Ground),
                (ESC, State::Esc),
            ] {
                let stream = [*opener, &[end]].concat();
                assert_eq!(walk(&stream), want, "{stream:?}");
            }
        }
    }

    #[test]
    fn bel_ends_an_osc_but_no_other_string() {
        assert_eq!(walk(b"\x1b]0;x\x07"), State::Ground);
        assert_eq!(walk(b"\x1bPqx\x07"), State::DcsPassthrough);
        assert_eq!(walk(b"\x1b_x\x07"), State::OpaqueString);
        assert_eq!(walk(b"\x1bXx\x07"), State::OpaqueString);
    }

    #[test]
    fn tokens_require_a_real_terminator() {
        assert_eq!(tokens(b"\x1b]a\x07"), [("osc", b"a".to_vec(), 0)]);
        assert_eq!(tokens(b"\x1b]a\x1b\\"), [("osc", b"a".to_vec(), 0)]);
        assert!(tokens(b"\x1b]a\x18").is_empty());
        assert!(tokens(b"\x1b]a\x1a").is_empty());
        // The ESC must be immediately followed by `\` to form ST.
        assert!(tokens(b"\x1b]a\x1b\n\\").is_empty());
        assert!(tokens(b"\x1b]a\x1b\x1b\\").is_empty());
        // A bare ST with no OSC before it is not an empty OSC.
        assert!(tokens(b"\x1b\\").is_empty());
    }

    #[test]
    fn csi_params_leave_out_what_the_emulator_ignores() {
        assert_eq!(tokens(b"\x1b[?6n"), [("csi", b"?6".to_vec(), b'n')]);
        // C0 controls and DEL inside a CSI execute or vanish; the CSI carries on.
        assert_eq!(tokens(b"\x1b[\n6\x7fn"), [("csi", b"6".to_vec(), b'n')]);
    }

    #[test]
    fn a_sequence_past_the_cap_is_dropped_whole_and_the_next_is_clean() {
        let mut long = b"\x1b]".to_vec();
        long.extend(std::iter::repeat_n(b'x', MAX_RESIDUAL + 1));
        long.push(BEL);
        assert!(tokens(&long).is_empty());

        long.extend_from_slice(b"\x1b]ok\x07");
        assert_eq!(tokens(&long), [("osc", b"ok".to_vec(), 0)]);
    }

    /// Records only what tells us the parser's state: printed characters.
    #[derive(Default)]
    struct Prints(Vec<char>);
    impl Perform for Prints {
        fn print(&mut self, c: char) {
            self.0.push(c);
        }
    }

    /// Small deterministic generator, so a failure reproduces from its stream.
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn byte(&mut self) -> u8 {
            // Mostly the bytes that move the state machine, some of anything.
            const LIKELY: &[u8] = b"\x1b\x1b\x1b\x1b[[]]PX^_\\\x07\x18\x1a0;?<$ (#mqZ\n\x7f\
                \x9c\xc3\xa9\xe0\xa0\xed\x80\x9f\xf0\x90\xf4\x8f\xbf\xff";
            let pick = self.next();
            if pick & 7 == 0 {
                (pick >> 8) as u8
            } else {
                LIKELY[(pick >> 8) as usize % LIKELY.len()]
            }
        }
    }

    /// vte hides its state, so probe it: from Ground a printable byte prints;
    /// mid-character it first flushes a replacement character; from anywhere
    /// else it's swallowed by the sequence.
    #[test]
    fn state_agrees_with_vte_on_random_streams() {
        let mut rng = XorShift(0x5e55_1011_dead_beef);
        for _ in 0..3000 {
            let stream: Vec<u8> = (0..40).map(|_| rng.byte()).collect();
            let mut state = State::Ground;
            for end in 0..=stream.len() {
                let mut parser = Parser::new();
                let mut seen = Prints::default();
                parser.advance(&mut seen, &stream[..end]);
                let before = seen.0.len();
                parser.advance(&mut seen, b"Z");
                let probe = &seen.0[before..];

                let agrees = match state {
                    State::Ground => probe == ['Z'],
                    State::Utf8 { .. } => probe == ['\u{fffd}', 'Z'],
                    _ => probe.is_empty(),
                };
                assert!(
                    agrees,
                    "after {:02x?}: we are in {state:?}, vte printed {probe:?}",
                    &stream[..end]
                );
                if let Some(&b) = stream.get(end) {
                    state = state.step(b);
                }
            }
        }
    }
}
