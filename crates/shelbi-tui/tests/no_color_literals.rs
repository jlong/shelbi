//! Every color the TUI paints comes from the theme (`src/theme.rs`, over
//! `shelbi_app::theme`), so the dark and light themes cover every surface.
//! This scans the crate's non-test source for a ratatui color literal outside
//! the theme conversion and fails on one.
//!
//! Allowed:
//! - `src/theme.rs`, the conversion itself;
//! - `src/shell/terminal_view.rs`, which converts a hosted program's own
//!   output colors (the emulator's) to ratatui, not the TUI's;
//! - `Color::Reset`, the terminal's default (no color at all);
//! - test modules, which assert on the colors a surface painted.

use std::path::{Path, PathBuf};

const ALLOWED_FILES: &[&str] = &["theme.rs", "shell/terminal_view.rs"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The file's text before its `#[cfg(test)] mod tests` block.
fn non_test_source(text: &str) -> &str {
    let mut offset = 0;
    let mut lines = text.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        if line.trim() == "#[cfg(test)]"
            && lines
                .peek()
                .is_some_and(|next| next.trim_start().starts_with("mod tests"))
        {
            return &text[..offset];
        }
        offset += line.len();
    }
    text
}

/// `Color::<Variant>` occurrences on `line`, other than `Color::Reset`, where
/// `Color` is a whole identifier (not `DecorationColor::`, `RatColor::`).
fn literals(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("Color::") {
        let preceded_by_ident = rest[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after = &rest[at + "Color::".len()..];
        let variant: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !preceded_by_ident && variant != "Reset" && !variant.is_empty() {
            found.push(format!("Color::{variant}"));
        }
        rest = after;
    }
    found
}

#[test]
fn no_color_literals_outside_the_theme() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();

    let mut hits = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
        if ALLOWED_FILES.contains(&rel.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        for (n, line) in non_test_source(&text).lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for lit in literals(line) {
                hits.push(format!("src/{rel}:{}: {lit}", n + 1));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "color literals outside the theme; use a `palette()` token instead \
         (add one to shelbi_app::theme if none fits):\n{}",
        hits.join("\n")
    );
}

#[test]
fn the_scanner_sees_literals() {
    assert_eq!(literals(".fg(Color::Rgb(1, 2, 3))"), vec!["Color::Rgb"]);
    assert_eq!(literals("Color::DarkGray, Color::Reset"), vec!["Color::DarkGray"]);
    assert!(literals("DecorationColor::Green => RatColor::Cyan").is_empty());
    assert_eq!(non_test_source("a\n#[cfg(test)]\nmod tests {\nColor::Red\n}"), "a\n");
}
