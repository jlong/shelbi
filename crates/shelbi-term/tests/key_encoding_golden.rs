//! Golden table for [`encode_key`]: every key this crate can name, under every
//! modifier set and every [`KeyEncoding`], against the bytes recorded in
//! `tests/fixtures/key_encoding_golden.tsv`.
//!
//! The fixture was recorded from the termwiz-backed encoder (termwiz 0.23.3's
//! `KeyCode::encode` plus this crate's plain-Tab-under-kitty override) before
//! termwiz was replaced by the crate's own encoder, so a pass here means the
//! replacement is byte-for-byte what shipped before it.
//!
//! To re-record after a deliberate encoding change, run
//! `SHELBI_WRITE_KEY_GOLDEN=1 cargo test -p shelbi-term --test key_encoding_golden`
//! and review the fixture diff: every changed line is a behavior change some
//! program on the other end of the PTY will see.

use shelbi_term::input::{encode_key, Key, KeyEncoding, Modifiers};

const FIXTURE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/key_encoding_golden.tsv");

/// Every key in the table: printable ASCII, the control characters a caller
/// might hand over as a `Char`, a few non-ASCII characters, every named key,
/// and F1..=F24 plus one past the end.
fn keys() -> Vec<Key> {
    let mut keys: Vec<Key> = ['\0', '\x08', '\t', '\r', '\x1b', '\x7f'].into_iter().map(Key::Char).collect();
    keys.extend((0x20u8..=0x7e).map(|b| Key::Char(b as char)));
    keys.extend(['é', 'É', 'ü', 'ß', 'λ', '€', '日', '😀'].into_iter().map(Key::Char));
    keys.extend([
        Key::Enter,
        Key::Tab,
        Key::Backspace,
        Key::Escape,
        Key::Delete,
        Key::Insert,
        Key::Up,
        Key::Down,
        Key::Left,
        Key::Right,
        Key::Home,
        Key::End,
        Key::PageUp,
        Key::PageDown,
    ]);
    keys.extend((1..=25).map(Key::Function));
    keys
}

fn key_label(key: Key) -> String {
    match key {
        Key::Char(c) => format!("U+{:04X}", c as u32),
        Key::Function(n) => format!("F{n}"),
        other => format!("{other:?}"),
    }
}

/// All 16 modifier sets, in bit order shift=1, alt=2, ctrl=4, super=8.
fn modifier_sets() -> Vec<Modifiers> {
    (0u8..16)
        .map(|bits| Modifiers {
            shift: bits & 1 != 0,
            alt: bits & 2 != 0,
            ctrl: bits & 4 != 0,
            sup: bits & 8 != 0,
        })
        .collect()
}

fn mods_label(m: Modifiers) -> String {
    let names: Vec<&str> = [(m.shift, "shift"), (m.alt, "alt"), (m.ctrl, "ctrl"), (m.sup, "super")]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join("+")
    }
}

/// All 8 encodings, in bit order newline_mode=1, application_cursor_keys=2,
/// kitty=4. These are the fixture's columns, left to right.
fn encodings() -> Vec<KeyEncoding> {
    (0u8..8)
        .map(|bits| KeyEncoding {
            newline_mode: bits & 1 != 0,
            application_cursor_keys: bits & 2 != 0,
            kitty: bits & 4 != 0,
        })
        .collect()
}

fn encoding_label(e: KeyEncoding) -> String {
    let mut s = String::from(if e.kitty { "kitty" } else { "legacy" });
    if e.application_cursor_keys {
        s.push_str("+ckm");
    }
    if e.newline_mode {
        s.push_str("+lnm");
    }
    s
}

/// One cell: `-` for no bytes; otherwise printable ASCII as itself, ESC as
/// `\e`, and every other byte (space, controls, non-ASCII, and the `\` and `-`
/// this format reserves) as `\xNN`.
fn escape(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "-".to_string();
    }
    let mut s = String::new();
    for &b in bytes {
        match b {
            0x1b => s.push_str("\\e"),
            b'\\' | b'-' => s.push_str(&format!("\\x{b:02x}")),
            0x21..=0x7e => s.push(b as char),
            _ => s.push_str(&format!("\\x{b:02x}")),
        }
    }
    s
}

fn unescape(cell: &str) -> Vec<u8> {
    if cell == "-" {
        return Vec::new();
    }
    let raw = cell.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] != b'\\' {
            out.push(raw[i]);
            i += 1;
            continue;
        }
        match raw.get(i + 1) {
            Some(b'e') => {
                out.push(0x1b);
                i += 2;
            }
            Some(b'x') => {
                let hex = std::str::from_utf8(&raw[i + 2..i + 4]).expect("hex escape");
                out.push(u8::from_str_radix(hex, 16).expect("hex escape"));
                i += 4;
            }
            other => panic!("bad escape {other:?} in cell {cell:?}"),
        }
    }
    out
}

/// The table as `encode_key` produces it today, one row per (key, modifiers).
fn render() -> String {
    let encs = encodings();
    let mut out = String::new();
    out.push_str("# Golden key-encoding table for shelbi_term::input::encode_key.\n");
    out.push_str("# First recorded from termwiz 0.23.3 (plus the plain-Tab-under-kitty override);\n");
    out.push_str("# see tests/key_encoding_golden.rs. Cells: `-` = no bytes, `\\e` = ESC,\n");
    out.push_str("# `\\xNN` = any other non-graphic byte (and `\\` / `-`), else the byte itself.\n");
    out.push_str("#key\tmods");
    for e in &encs {
        out.push('\t');
        out.push_str(&encoding_label(*e));
    }
    out.push('\n');
    for key in keys() {
        for mods in modifier_sets() {
            out.push_str(&key_label(key));
            out.push('\t');
            out.push_str(&mods_label(mods));
            for e in &encs {
                out.push('\t');
                out.push_str(&escape(&encode_key(key, mods, *e)));
            }
            out.push('\n');
        }
    }
    out
}

#[test]
fn encoder_matches_the_golden_table_byte_for_byte() {
    if std::env::var_os("SHELBI_WRITE_KEY_GOLDEN").is_some() {
        std::fs::write(FIXTURE_PATH, render()).expect("write golden fixture");
        return;
    }
    let fixture = std::fs::read_to_string(FIXTURE_PATH).expect("read golden fixture");
    let encs = encodings();

    // Index the fixture, then walk the enumeration so a key missing from the
    // fixture (or a fixture row with no key) fails as loudly as a wrong byte.
    let mut rows = std::collections::BTreeMap::new();
    for line in fixture.lines().filter(|l| !l.starts_with('#')) {
        let cells: Vec<&str> = line.split('\t').collect();
        assert_eq!(cells.len(), 2 + encs.len(), "malformed fixture row: {line:?}");
        let prev = rows.insert((cells[0].to_string(), cells[1].to_string()), cells[2..].to_vec());
        assert!(prev.is_none(), "duplicate fixture row: {line:?}");
    }

    let mut mismatches = Vec::new();
    let mut checked = 0;
    for key in keys() {
        for mods in modifier_sets() {
            let id = (key_label(key), mods_label(mods));
            let cells = rows.remove(&id).unwrap_or_else(|| panic!("fixture has no row for {id:?}"));
            for (e, cell) in encs.iter().zip(cells) {
                let want = unescape(cell);
                let got = encode_key(key, mods, *e);
                checked += 1;
                if got != want {
                    mismatches.push(format!(
                        "{} {} {}: want {} got {}",
                        id.0,
                        id.1,
                        encoding_label(*e),
                        escape(&want),
                        escape(&got)
                    ));
                }
            }
        }
    }
    assert!(rows.is_empty(), "fixture rows with no key in the enumeration: {:?}", rows.keys());
    assert!(
        mismatches.is_empty(),
        "{} of {checked} encodings differ from the golden table:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn cell_escaping_round_trips() {
    for bytes in [&b""[..], b"-", b"\\", b" ", b"\x1b[13;2u", b"\x00\x7f\xc3\xa9", b"a"] {
        assert_eq!(unescape(&escape(bytes)), bytes);
    }
}
