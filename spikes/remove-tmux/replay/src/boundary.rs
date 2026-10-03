//! Case (c): split the live stream only where the parser is at rest.
//!
//! Output is split into sequenced frames, and a reconnecting client resumes at
//! a frame edge after replay has reset its parser to Ground. So a frame
//! boundary must never fall inside an escape sequence or a multi-byte UTF-8
//! character, or the bytes straddling it are misparsed. This module finds the
//! at-rest offsets and quantifies how badly naive fixed-size framing tears a
//! real stream.

/// Parser rest-state tracker: enough of a VT state machine to know when the
/// parser sits at a token boundary (Ground, no pending UTF-8 continuation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
            0x07 => St::Ground,                        // BEL terminates
            b'\\' if seen_esc => St::Ground,           // ST (ESC \)
            0x1b => St::Str(true),
            _ => St::Str(false),
        },
        St::Charset => St::Ground,
    }
}

/// `rest[i]` is true when the parser is at rest *before* byte `i`; `rest[len]`
/// reflects the state after the whole stream. Offsets 0 and (for a clean
/// stream) `len` are always safe frame edges.
pub fn rest_mask(bytes: &[u8]) -> Vec<bool> {
    let mut rest = Vec::with_capacity(bytes.len() + 1);
    let mut st = St::Ground;
    for &b in bytes {
        rest.push(st == St::Ground);
        st = step(st, b);
    }
    rest.push(st == St::Ground);
    rest
}

/// Is a cut at `offset` a torn frame edge (inside a sequence or UTF-8 char)?
pub fn tears_at(bytes: &[u8], offset: usize) -> bool {
    if offset == 0 || offset >= bytes.len() {
        return false;
    }
    !rest_mask(bytes)[offset]
}

/// How many fixed-size frame edges (`chunk`, 2*chunk, ...) tear the stream.
pub fn count_fixed_tears(bytes: &[u8], chunk: usize) -> usize {
    let mut n = 0;
    let mut off = chunk;
    while off < bytes.len() {
        if tears_at(bytes, off) {
            n += 1;
        }
        off += chunk;
    }
    n
}

/// Split into frames of roughly `target` bytes, each ending at the next
/// at-rest offset at or after the target. Never tears a sequence.
pub fn rest_chunks(bytes: &[u8], target: usize) -> Vec<&[u8]> {
    let rest = rest_mask(bytes);
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = (start + target).min(bytes.len());
        while end < bytes.len() && !rest[end] {
            end += 1;
        }
        chunks.push(&bytes[start..end]);
        start = end;
    }
    chunks
}
