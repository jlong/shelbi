// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-cli/src/lib.rs), MIT in Shelbi.
// `render_write_output`, `render_new_document`, `append_to_document`, the
// head/tail slicing and the `ls --stats` counts, lifted out of the CLI so the
// hub applies them (it is the wiki's single writer) and `shelbi wiki` prints
// them. The section edits and `prepend_to_document` below the copied code
// are Shelbi's own (spacing and the H1 kept first).

//! Whole-document edits with `cstore`'s rules: a write keeps the
//! document's frontmatter and H1 unless the new content brings its own,
//! a new document gets an H1 from its title, and an append lands on a line
//! of its own. Plus the line/word counts `ls --stats` shows.

use crate::markdown::sections::{self, extract_frontmatter, find_section, list_headings};

/// The document a write of `new_content` over `existing` leaves.
///
/// - The document's frontmatter is kept unless `new_content` has its own.
/// - With `title`, the H1 is `# <title>` (a leading H1 in `new_content` is
///   dropped: the title wins).
/// - Otherwise `new_content`'s own H1 is used when it has one, else the
///   document's H1 is kept.
pub fn render_write_output(existing: &str, new_content: &str, title: Option<&str>) -> String {
    let (existing_frontmatter, existing_body) = extract_frontmatter(existing);
    let (new_frontmatter, new_body) = extract_frontmatter(new_content);

    let new_body_trimmed = new_body.trim_matches(|c: char| c.is_whitespace()).to_string();
    let new_has_h1 = new_body_trimmed.starts_with("# ");

    let existing_h1: Option<String> = {
        let body = existing_body.trim_matches(|c: char| c.is_whitespace());
        if body.starts_with("# ") {
            body.split('\n').next().map(str::to_string)
        } else {
            None
        }
    };

    let final_h1: Option<String> = if let Some(t) = title {
        Some(format!("# {t}"))
    } else if new_has_h1 {
        None
    } else {
        existing_h1
    };

    let body_content: String = if new_has_h1 && title.is_some() {
        new_body_trimmed
            .split_once('\n')
            .map(|x| x.1)
            .unwrap_or("")
            .trim_matches('\n')
            .to_string()
    } else {
        new_body_trimmed.clone()
    };

    let frontmatter = new_frontmatter.or(existing_frontmatter);

    let mut result = String::new();
    if let Some(fm) = frontmatter {
        result.push_str(&format!("---\n{fm}\n---\n\n"));
    }
    // Upstream pushed no break after a `title` H1 when the new content had
    // its own H1, gluing the title to the body (`# MineBody.`); here the
    // H1 always ends its line, with a blank line before any body.
    if let Some(h1) = &final_h1 {
        result.push_str(h1);
        result.push_str(if body_content.is_empty() { "\n" } else { "\n\n" });
    }
    result.push_str(&body_content);
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// A new document titled `title` holding `content`: frontmatter (if
/// `content` has some) first, then `# <title>` unless `content` starts with
/// an H1 of its own, then the content.
pub fn render_new_document(title: &str, content: &str) -> String {
    let (frontmatter, body) = extract_frontmatter(content);
    let trimmed_body = body.trim_matches(|c: char| c.is_whitespace()).to_string();
    match frontmatter {
        Some(fm) if trimmed_body.is_empty() => format!("---\n{fm}\n---\n\n# {title}\n"),
        Some(fm) if trimmed_body.starts_with("# ") => format!("---\n{fm}\n---\n\n{trimmed_body}\n"),
        Some(fm) => format!("---\n{fm}\n---\n\n# {title}\n\n{trimmed_body}\n"),
        None if content.starts_with("# ") => ensure_newline(content.to_string()),
        None if content.trim().is_empty() => format!("# {title}\n"),
        None => ensure_newline(format!("# {title}\n\n{content}")),
    }
}

/// `new_content` added at the end of `existing`, on a line of its own.
pub fn append_to_document(existing: &str, new_content: &str) -> String {
    let joined = if existing.is_empty() || existing.ends_with('\n') {
        format!("{existing}{new_content}")
    } else {
        format!("{existing}\n{new_content}")
    };
    ensure_newline(joined)
}

/// Replace the body of the section under `heading` with `body`, keeping
/// its heading: a blank line after the heading and, when another heading
/// follows, before it. (The section reaches the next heading of its level
/// or higher, so its subsections go too.)
pub fn write_section(content: &str, heading: &str, body: &str) -> crate::markdown::Result<String> {
    let section = find_section(content, heading)?;
    let mut lines: Vec<&str> = content.lines().collect();
    let start = section.start_line.min(lines.len());
    let end = (section.end_line - 1).clamp(start, lines.len());
    let body = body.trim_matches('\n');
    let mut block: Vec<&str> = Vec::new();
    if !body.is_empty() {
        block.push("");
        block.extend(body.lines());
    }
    if end < lines.len() {
        block.push("");
    }
    lines.splice(start..end, block);
    Ok(finish(&lines))
}

/// Insert `addition` at the start of the section under `heading`, after
/// the heading and a blank line, with a blank line before what follows.
pub fn prepend_to_section(content: &str, heading: &str, addition: &str) -> crate::markdown::Result<String> {
    let section = find_section(content, heading)?;
    let mut lines: Vec<&str> = content.lines().collect();
    let start = section.start_line.min(lines.len());
    let end = (section.end_line - 1).clamp(start, lines.len());
    let mut first = start;
    while first < end && lines[first].trim().is_empty() {
        first += 1;
    }
    let mut block: Vec<&str> = vec![""];
    block.extend(addition.trim_matches('\n').lines());
    if first < lines.len() {
        block.push("");
    }
    lines.splice(start..first, block);
    Ok(finish(&lines))
}

/// Add `addition` at the end of the section under `heading`, before the
/// blank lines that lead to the next heading (a list item joins the list).
pub fn append_to_section(content: &str, heading: &str, addition: &str) -> crate::markdown::Result<String> {
    sections::append_to_section(content, heading, addition.trim_matches('\n')).map(ensure_newline)
}

/// Remove the section under `heading` (its heading and body).
pub fn remove_section(content: &str, heading: &str) -> crate::markdown::Result<String> {
    sections::remove_section(content, heading).map(ensure_newline)
}

/// Insert `addition` at the start of the document: after its frontmatter
/// and its H1 when it starts with one (so the title stays the first line),
/// else after the frontmatter. A blank line separates it from both sides.
pub fn prepend_to_document(content: &str, addition: &str) -> String {
    let (_, body) = extract_frontmatter(content);
    let prefix = &content[..content.len() - body.len()];
    let addition = addition.trim_matches('\n');
    let trimmed = body.trim_start_matches(['\n', '\r']);
    let leading = &body[..body.len() - trimmed.len()];
    let first_line = trimmed.lines().next().unwrap_or("");
    if first_line.starts_with("# ") {
        let rest = trimmed[first_line.len()..].trim_start_matches(['\n', '\r']);
        let tail = if rest.is_empty() { String::new() } else { format!("\n{rest}") };
        ensure_newline(format!("{prefix}{leading}{first_line}\n\n{addition}\n{tail}"))
    } else if trimmed.is_empty() {
        ensure_newline(format!("{prefix}{addition}"))
    } else {
        ensure_newline(format!("{prefix}{addition}\n\n{trimmed}"))
    }
}

/// Lines back into a document ending in a newline.
fn finish(lines: &[&str]) -> String {
    ensure_newline(lines.join("\n"))
}

fn ensure_newline(mut s: String) -> String {
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// The first `head` or last `tail` lines of `content` (`tail` wins when both
/// are given); all of it when neither is.
pub fn head_tail(content: &str, head: Option<usize>, tail: Option<usize>) -> String {
    let lines: Vec<&str> = content.split('\n').collect();
    let slice: &[&str] = if let Some(n) = tail {
        // A trailing newline isn't a line of its own.
        let len = if content.ends_with('\n') { lines.len() - 1 } else { lines.len() };
        &lines[len.saturating_sub(n)..len]
    } else if let Some(n) = head {
        &lines[..n.min(lines.len())]
    } else {
        return content.to_string();
    };
    slice.join("\n")
}

/// Line, word and byte counts of a document, as `ls --stats` shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Lines (`split('\n')`, as `cstore` counts them).
    pub lines: usize,
    /// Whitespace-separated words.
    pub words: usize,
    /// Size in bytes.
    pub bytes: u64,
}

/// [`Counts`] for `content`.
pub fn counts(content: &str) -> Counts {
    Counts {
        lines: content.split('\n').count(),
        words: content.split_whitespace().count(),
        bytes: content.len() as u64,
    }
}

/// One section of a document with its counts, from its heading to the next
/// heading of any level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionCounts {
    /// The heading line as written (`## Goals`).
    pub heading: String,
    /// The heading's text, without its `#`s.
    pub text: String,
    /// 1..=6.
    pub level: u8,
    /// Lines from the heading to the next one.
    pub lines: usize,
    /// Words in those lines.
    pub words: usize,
}

/// Every heading of `content` with the size of the text under it.
pub fn section_counts(content: &str) -> Vec<SectionCounts> {
    let headings = list_headings(content);
    let lines: Vec<&str> = content.split('\n').collect();
    let mut out = Vec::with_capacity(headings.len());
    for (i, h) in headings.iter().enumerate() {
        let start = h.line.saturating_sub(1);
        let end = headings.get(i + 1).map_or(lines.len(), |next| next.line.saturating_sub(1));
        if start >= end {
            continue;
        }
        let slice = &lines[start..end];
        out.push(SectionCounts {
            heading: h.raw_line.clone(),
            text: h.text.clone(),
            level: h.level,
            lines: slice.len(),
            words: slice.join(" ").split_whitespace().count(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_without_an_h1_keeps_the_frontmatter_and_title() {
        let existing = "---\nstatus: draft\n---\n\n# Plan\n\nOld body.\n";
        let out = render_write_output(existing, "New body.", None);
        assert_eq!(out, "---\nstatus: draft\n---\n\n# Plan\n\nNew body.\n");
    }

    #[test]
    fn a_write_with_its_own_h1_and_frontmatter_replaces_them() {
        let existing = "---\nstatus: draft\n---\n\n# Plan\n\nOld.\n";
        let out = render_write_output(existing, "---\nstatus: done\n---\n# Renamed\n\nNew.\n", None);
        assert_eq!(out, "---\nstatus: done\n---\n\n# Renamed\n\nNew.\n");
    }

    #[test]
    fn a_title_wins_over_the_contents_h1() {
        let out = render_write_output("# Old\n\nx\n", "# Theirs\n\nBody.", Some("Mine"));
        assert_eq!(out, "# Mine\n\nBody.\n");
    }

    #[test]
    fn a_new_document_gets_a_title_unless_it_brings_one() {
        assert_eq!(render_new_document("API Design", ""), "# API Design\n");
        assert_eq!(render_new_document("API Design", "Notes."), "# API Design\n\nNotes.\n");
        assert_eq!(render_new_document("X", "# Own\n\nBody"), "# Own\n\nBody\n");
        assert_eq!(
            render_new_document("X", "---\na: 1\n---\nBody"),
            "---\na: 1\n---\n\n# X\n\nBody\n"
        );
    }

    #[test]
    fn an_append_lands_on_its_own_line() {
        assert_eq!(append_to_document("# A\n", "- one"), "# A\n- one\n");
        assert_eq!(append_to_document("# A", "- one"), "# A\n- one\n");
    }

    #[test]
    fn a_section_write_keeps_the_heading_and_spacing() {
        let doc = "---\na: 1\n---\n# T\n\n## Notes\n\nOld.\nOlder.\n\n## Next\n\nKeep.\n";
        assert_eq!(
            write_section(doc, "## Notes", "New.").unwrap(),
            "---\na: 1\n---\n# T\n\n## Notes\n\nNew.\n\n## Next\n\nKeep.\n"
        );
        assert_eq!(write_section("# T\n\n## Last\nx", "## Last", "y\n").unwrap(), "# T\n\n## Last\n\ny\n");
        assert!(write_section(doc, "## Gone", "x").is_err());
    }

    #[test]
    fn a_section_prepend_lands_after_its_heading() {
        let doc = "# T\n\n## Notes\n\nOld.\n\n## Next\n";
        assert_eq!(
            prepend_to_section(doc, "## Notes", "First.").unwrap(),
            "# T\n\n## Notes\n\nFirst.\n\nOld.\n\n## Next\n"
        );
        assert_eq!(
            prepend_to_section("## Empty\n## Next\n", "## Empty", "x").unwrap(),
            "## Empty\n\nx\n\n## Next\n"
        );
    }

    #[test]
    fn a_document_prepend_keeps_the_frontmatter_and_title_first() {
        assert_eq!(
            prepend_to_document("---\na: 1\n---\n\n# T\n\nBody.\n", "> Summary."),
            "---\na: 1\n---\n\n# T\n\n> Summary.\n\nBody.\n"
        );
        assert_eq!(prepend_to_document("# T\n", "x"), "# T\n\nx\n");
        assert_eq!(prepend_to_document("Body.\n", "x"), "x\n\nBody.\n");
        assert_eq!(prepend_to_document("---\na: 1\n---\nBody.", "x"), "---\na: 1\n---\nx\n\nBody.\n");
    }

    #[test]
    fn section_appends_and_removals_end_in_a_newline() {
        let doc = "# T\n\n## A\n\n- one\n\n## B\n\nb\n";
        assert_eq!(append_to_section(doc, "## A", "- two").unwrap(), "# T\n\n## A\n\n- one\n- two\n\n## B\n\nb\n");
        assert_eq!(remove_section(doc, "## B").unwrap(), "# T\n\n## A\n\n- one\n");
    }

    #[test]
    fn head_and_tail_slice_lines() {
        let text = "a\nb\nc\nd\n";
        assert_eq!(head_tail(text, Some(2), None), "a\nb");
        assert_eq!(head_tail(text, None, Some(2)), "c\nd");
        assert_eq!(head_tail(text, Some(1), Some(1)), "d");
        assert_eq!(head_tail(text, None, None), text);
    }

    #[test]
    fn sections_are_counted_to_the_next_heading() {
        let doc = "# T\n\nintro words\n\n## A\n\none two three\n";
        let s = section_counts(doc);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].heading.as_str(), s[0].level, s[0].lines, s[0].words), ("# T", 1, 4, 4));
        assert_eq!((s[1].heading.as_str(), s[1].words), ("## A", 5));
        assert_eq!(counts(doc).words, 9);
    }
}
