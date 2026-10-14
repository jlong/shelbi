//! The bench's inputs: recorded or generated terminal output.
//!
//! - `claude`: a captured Claude Code session with heavy tool output
//!   (`benches/corpus/claude-code.cap.gz`, recorded with `--bench-record`
//!   from a live worker session: its full-screen renderer redrawing the
//!   transcript, tool calls, and their output).
//! - `yes`: what `yes | head -1000000` writes, a scroll per line.
//! - `cargo`: a colored `cargo build` log, generated in cargo's own escape
//!   sequences (bold green `Compiling`, yellow `warning:` blocks with blue
//!   gutters, and the `Building [===>  ]` progress line redrawn in place with
//!   `\r` and erase-line between them).
//! - `redraw`: a full-screen redraw loop on the alternate screen (`htop` /
//!   `btop` style): every frame homes the cursor and repaints every cell with
//!   true-color backgrounds and changing text.
//!
//! Generated inputs are deterministic, so before/after runs feed the same
//! bytes. Each is written several times over ([`Corpus::repeat_for`]) so a
//! case streams for a few seconds.

use std::io::Read;
use std::path::{Path, PathBuf};

use shelbi_term::Size;

/// The first line of a capture file.
pub const CAPTURE_MAGIC: &str = "SHELBI-CAPTURE";

/// One bench input.
#[derive(Debug, Clone)]
pub struct Corpus {
    pub name: &'static str,
    /// The session size the output was written for.
    pub size: Size,
    pub bytes: Vec<u8>,
}

impl Corpus {
    fn new(name: &'static str, size: Size, bytes: Vec<u8>) -> Corpus {
        Corpus { name, size, bytes }
    }

    /// How many times to write the input back to back for about `total`
    /// bytes in all (at least once).
    pub fn repeat_for(&self, total: usize) -> usize {
        (total / self.bytes.len().max(1)).max(1)
    }
}

/// The input names, in report order.
pub const NAMES: [&str; 4] = ["claude", "yes", "cargo", "redraw"];

/// The size the generated inputs are written for (a typical pane).
pub const GENERATED_SIZE: Size = Size { cols: 120, rows: 40 };

/// Load or generate input `name`.
pub fn load(name: &str) -> Result<Corpus, String> {
    match name {
        "claude" => {
            let (size, bytes) = read_capture(&capture_path())?;
            Ok(Corpus::new("claude", size, bytes))
        }
        "yes" => Ok(Corpus::new("yes", GENERATED_SIZE, yes(1_000_000))),
        "cargo" => Ok(Corpus::new(
            "cargo",
            GENERATED_SIZE,
            cargo(GENERATED_SIZE.cols as usize, 1500),
        )),
        "redraw" => Ok(Corpus::new(
            "redraw",
            GENERATED_SIZE,
            redraw(GENERATED_SIZE, 1500),
        )),
        other => Err(format!(
            "unknown input `{other}` (one of {})",
            NAMES.join(", ")
        )),
    }
}

/// Where the recorded Claude Code session lives.
pub fn capture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("benches/corpus/claude-code.cap.gz")
}

/// Read a capture written by `--bench-record`.
pub fn read_capture(path: &Path) -> Result<(Size, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(file)
        .read_to_end(&mut raw)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let newline = raw
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| format!("{}: no header", path.display()))?;
    let header = std::str::from_utf8(&raw[..newline]).map_err(|e| e.to_string())?;
    let mut words = header.split_whitespace();
    if words.next() != Some(CAPTURE_MAGIC) {
        return Err(format!("{}: not a capture", path.display()));
    }
    let mut size = GENERATED_SIZE;
    for word in words {
        match word.split_once('=') {
            Some(("cols", v)) => size.cols = v.parse().map_err(|_| "bad cols")?,
            Some(("rows", v)) => size.rows = v.parse().map_err(|_| "bad rows")?,
            _ => {}
        }
    }
    Ok((size, raw[newline + 1..].to_vec()))
}

/// `yes | head -<lines>` as a terminal receives it (the PTY turns `\n` into
/// `\r\n`).
pub fn yes(lines: usize) -> Vec<u8> {
    b"y\r\n".repeat(lines)
}

/// A small deterministic generator (xorshift), so the generated inputs don't
/// need a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const WORDS: [&str; 24] = [
    "serde", "tokio", "futures", "regex", "syn", "quote", "proc", "macro", "core", "shelbi",
    "term", "session", "client", "proto", "state", "app", "palette", "orchestrator", "agent",
    "ssh", "tui", "alacritty", "unicode", "width",
];

/// A colored `cargo build` log: `crates` crates compiled, a warning block
/// every few, and the progress line redrawn between them.
pub fn cargo(cols: usize, crates: usize) -> Vec<u8> {
    let mut rng = Rng(0x5eed_cafe);
    let mut out = Vec::new();
    let green = "\x1b[1m\x1b[32m";
    let yellow = "\x1b[1m\x1b[33m";
    let blue = "\x1b[1m\x1b[38;5;12m";
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";
    for i in 0..crates {
        // Clear the progress line, print the next finished crate.
        out.extend_from_slice(b"\r\x1b[K");
        let name = format!("{}-{}", rng.pick(&WORDS), rng.pick(&WORDS));
        let version = format!("{}.{}.{}", rng.below(3), rng.below(40), rng.below(20));
        out.extend_from_slice(
            format!("{green}   Compiling{reset} {name} v{version}\r\n").as_bytes(),
        );
        if i % 7 == 3 {
            let file = format!("src/{}/{}.rs", rng.pick(&WORDS), rng.pick(&WORDS));
            let line = 10 + rng.below(900);
            let col = 1 + rng.below(40);
            let ident = rng.pick(&WORDS);
            out.extend_from_slice(
                format!(
                    "{yellow}warning{reset}{bold}: unused variable: `{ident}`{reset}\r\n\
                     {blue}  --> {reset}{file}:{line}:{col}\r\n\
                     {blue}   |{reset}\r\n\
                     {blue}{line:>3} |{reset}     let {ident} = {}::new(&{}, {});\r\n\
                     {blue}   |{reset}         {yellow}{}{reset} {yellow}help: if this is intentional, prefix it with an underscore: `_{ident}`{reset}\r\n\
                     {blue}   |{reset}\r\n\
                     {blue}   = {reset}{bold}note{reset}: `#[warn(unused_variables)]` on by default\r\n\r\n",
                    rng.pick(&WORDS),
                    rng.pick(&WORDS),
                    rng.below(100),
                    "^".repeat(ident.len()),
                )
                .as_bytes(),
            );
        }
        // The progress line: `    Building [=====>      ] 123/456: a, b, c`.
        let done = i + 1;
        let width = 27;
        let filled = done * width / crates;
        let bar = format!(
            "{}>{}",
            "=".repeat(filled.saturating_sub(1)),
            " ".repeat(width - filled.max(1))
        );
        let mut line = format!(
            "{}    Building{} [{bar}] {done}/{crates}: {}, {}, {}",
            "\x1b[1m\x1b[36m",
            reset,
            rng.pick(&WORDS),
            rng.pick(&WORDS),
            rng.pick(&WORDS)
        );
        line.truncate(line.len().min(cols + 20));
        out.extend_from_slice(line.as_bytes());
    }
    out.extend_from_slice(
        format!("\r\x1b[K{green}    Finished{reset} `dev` profile [unoptimized + debuginfo] target(s) in 1m 32s\r\n")
            .as_bytes(),
    );
    out
}

/// A full-screen redraw loop: `frames` frames on the alternate screen, each
/// repainting every cell (a header bar, a table of rows with true-color
/// meters, a footer).
pub fn redraw(size: Size, frames: usize) -> Vec<u8> {
    let mut rng = Rng(0xfeed_beef);
    let (cols, rows) = (size.cols as usize, size.rows as usize);
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[?1049h\x1b[?25l");
    for frame in 0..frames {
        out.extend_from_slice(b"\x1b[H");
        // Header.
        let title = format!(" bench redraw  frame {frame:>5}  load {:.2}", rng.below(800) as f32 / 100.0);
        out.extend_from_slice(b"\x1b[48;2;40;40;40m\x1b[38;2;230;230;230m\x1b[1m");
        out.extend_from_slice(pad(&title, cols).as_bytes());
        out.extend_from_slice(b"\x1b[0m");
        for row in 1..rows - 1 {
            let pid = 1000 + row * 7 + frame % 13;
            let cpu = rng.below(1000) as f32 / 10.0;
            let meter = (cpu / 100.0 * 30.0) as usize;
            let shade = 60 + (row * 4 % 120) as u8;
            let name = rng.pick(&WORDS);
            let text = format!(
                " {pid:>6}  {cpu:>5.1}%  {:<12} ",
                name
            );
            out.extend_from_slice(format!("\r\n\x1b[38;2;200;200;200m{text}").as_bytes());
            out.extend_from_slice(
                format!("\x1b[48;2;{shade};{};80m{}\x1b[48;2;30;30;30m{}\x1b[0m", 200 - shade / 2, " ".repeat(meter), " ".repeat(30 - meter))
                    .as_bytes(),
            );
            let used = text.chars().count() + 30;
            let tail: String = (0..cols.saturating_sub(used))
                .map(|i| (b'a' + ((i + frame + row) % 26) as u8) as char)
                .collect();
            out.extend_from_slice(format!("\x1b[38;5;{}m{tail}\x1b[0m", 16 + (row + frame) % 216).as_bytes());
        }
        // Footer.
        out.extend_from_slice(b"\r\n\x1b[7m");
        out.extend_from_slice(pad(" F1 Help  F2 Setup  F3 Search  F10 Quit", cols).as_bytes());
        out.extend_from_slice(b"\x1b[0m");
    }
    out.extend_from_slice(b"\x1b[?25h");
    out
}

fn pad(s: &str, cols: usize) -> String {
    let mut s: String = s.chars().take(cols).collect();
    let n = s.chars().count();
    s.extend(std::iter::repeat_n(' ', cols - n));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_inputs_are_deterministic_and_fill_the_screen() {
        assert_eq!(cargo(120, 50), cargo(120, 50));
        assert_eq!(redraw(GENERATED_SIZE, 3), redraw(GENERATED_SIZE, 3));
        let mut view = shelbi_term::view::TerminalView::new(GENERATED_SIZE);
        view.feed(&redraw(GENERATED_SIZE, 2));
        let rows = view.screen_text().rows;
        assert_eq!(rows.len(), GENERATED_SIZE.rows as usize);
        assert!(rows[0].contains("frame     1"), "{rows:?}");
        assert!(rows.iter().all(|r| !r.trim().is_empty()), "every row painted: {rows:?}");
    }
}
