// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-markdown/src/sections.rs), MIT in Shelbi.

//! Heading addressing, title and frontmatter extraction, plus
//! section read/find. write/append/prepend/remove operations land
//! in a follow-up commit on the same branch.

use comrak::nodes::{AstNode, NodeValue};
use comrak::Arena;

use crate::markdown::comrak_options;

/// A markdown heading, located in the source document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// Heading level (1..=6).
    pub level: u8,

    /// Text of the heading, with no `#` prefix and no trailing newline.
    /// Inline formatting (emphasis, code, links) is rendered to plain
    /// text so callers can match against natural strings.
    pub text: String,

    /// The full heading line as it appears in the source, including the
    /// `#` prefix and any trailing whitespace stripped. Matches what
    /// the Swift app exposes as `Heading.text`.
    pub raw_line: String,

    /// 1-based line index of the heading line in the source document.
    /// Matches the line numbers users see in editors.
    pub line: usize,
}

/// A located section: a heading and the line range its body occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The heading the section starts at.
    pub heading: Heading,

    /// 1-based, inclusive line index of the heading line.
    pub start_line: usize,

    /// 1-based, exclusive line index of where the section ends — the
    /// line of the next sibling/parent heading, or one past the last
    /// line when this section runs to EOF.
    pub end_line: usize,
}

/// Every heading in `content`, in source order.
///
/// Headings inside fenced code blocks are excluded — comrak parses
/// them as text, not as headings. Headings inside YAML frontmatter
/// are also excluded (the frontmatter extension consumes the block).
pub fn list_headings(content: &str) -> Vec<Heading> {
    let arena = Arena::new();
    let options = comrak_options();
    let root = comrak::parse_document(&arena, content, &options);
    let lines: Vec<&str> = content.lines().collect();
    let mut headings = Vec::new();
    for node in root.descendants() {
        let data = node.data.borrow();
        if let NodeValue::Heading(h) = &data.value {
            let line = data.sourcepos.start.line;
            let raw_line = lines
                .get(line.saturating_sub(1))
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            headings.push(Heading {
                level: h.level,
                text: heading_text(node),
                raw_line,
                line,
            });
        }
    }
    headings
}

/// Find the section starting at `heading_str` in `content`.
///
/// `heading_str` is the full heading line as the user would type it,
/// e.g. `"### Q2 Goals"`. Matching compares level AND text-after-#-prefix
/// exactly. Returns:
///
///   * [`crate::markdown::MarkdownError::SectionNotFound`] if no heading matches.
///   * [`crate::markdown::MarkdownError::AmbiguousSection`] if more than one matches.
pub fn find_section(content: &str, heading_str: &str) -> crate::markdown::Result<Section> {
    let (level, target_text) = parse_heading_string(heading_str)
        .ok_or_else(|| crate::markdown::MarkdownError::SectionNotFound(heading_str.to_string()))?;

    let headings = list_headings(content);
    let matches: Vec<usize> = headings
        .iter()
        .enumerate()
        .filter(|(_, h)| h.level == level && h.text == target_text)
        .map(|(i, _)| i)
        .collect();

    match matches.len() {
        0 => Err(crate::markdown::MarkdownError::SectionNotFound(
            heading_str.to_string(),
        )),
        1 => {
            let idx = matches[0];
            let heading = headings[idx].clone();
            let start_line = heading.line;
            let total_lines = content.lines().count();
            let end_line = headings[idx + 1..]
                .iter()
                .find(|h| h.level <= level)
                .map(|h| h.line)
                .unwrap_or(total_lines + 1);
            Ok(Section {
                heading,
                start_line,
                end_line,
            })
        }
        n => Err(crate::markdown::MarkdownError::AmbiguousSection {
            heading: heading_str.to_string(),
            count: n,
        }),
    }
}

/// Read the text of the section identified by `heading_str` — the
/// heading line plus its body, with trailing blank lines trimmed.
pub fn read_section(content: &str, heading_str: &str) -> crate::markdown::Result<String> {
    let section = find_section(content, heading_str)?;
    let lines: Vec<&str> = content.lines().collect();
    let start = section.start_line.saturating_sub(1);
    let end = section.end_line.saturating_sub(1).min(lines.len());
    let mut slice: Vec<&str> = lines[start..end].to_vec();
    while slice.last().is_some_and(|l| l.trim().is_empty()) {
        slice.pop();
    }
    Ok(slice.join("\n"))
}

/// Replace the body of the named section with `new_body`. The heading
/// line is preserved.
pub fn write_section(content: &str, heading_str: &str, new_body: &str) -> crate::markdown::Result<String> {
    let section = find_section(content, heading_str)?;
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let body_start = section.start_line; // 0-based first body line
    let body_end = (section.end_line - 1).min(lines.len()); // exclusive
    if body_start < body_end {
        lines.drain(body_start..body_end);
    }
    for (i, new_line) in new_body.lines().enumerate() {
        lines.insert(body_start + i, new_line.to_string());
    }
    Ok(lines.join("\n"))
}

/// Append `new_content` at the end of the named section, before any
/// trailing blank lines that precede the next sibling/parent heading.
pub fn append_to_section(
    content: &str,
    heading_str: &str,
    new_content: &str,
) -> crate::markdown::Result<String> {
    let section = find_section(content, heading_str)?;
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let mut insert_at = (section.end_line - 1).min(lines.len());
    let heading_idx = section.start_line; // 0-based heading line
    while insert_at > heading_idx + 1 && lines[insert_at - 1].trim().is_empty() {
        insert_at -= 1;
    }
    for (i, new_line) in new_content.lines().enumerate() {
        lines.insert(insert_at + i, new_line.to_string());
    }
    Ok(lines.join("\n"))
}

/// Prepend `new_content` immediately after the section's heading line.
pub fn prepend_to_section(
    content: &str,
    heading_str: &str,
    new_content: &str,
) -> crate::markdown::Result<String> {
    let section = find_section(content, heading_str)?;
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let insert_at = section.start_line; // 0-based first body line
    for (i, new_line) in new_content.lines().enumerate() {
        lines.insert(insert_at + i, new_line.to_string());
    }
    Ok(lines.join("\n"))
}

/// Remove the named section entirely (heading + body), collapsing
/// pairs of blank lines left behind so the document doesn't grow
/// extra vertical whitespace.
pub fn remove_section(content: &str, heading_str: &str) -> crate::markdown::Result<String> {
    let section = find_section(content, heading_str)?;
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let start = section.start_line.saturating_sub(1);
    let end = (section.end_line - 1).min(lines.len());
    lines.drain(start..end);
    if start > 0
        && start < lines.len()
        && lines[start - 1].trim().is_empty()
        && lines[start].trim().is_empty()
    {
        lines.remove(start);
    }
    Ok(lines.join("\n"))
}

/// Insert `new_content` immediately after the YAML frontmatter (if
/// present) or at the very top of the document otherwise.
///
/// When frontmatter is present, the result has the shape
/// `---\n<frontmatter>\n---\n<new_content>\n<body>` — matching the
/// Swift app's `prependToDocument` output.
pub fn prepend_to_document(content: &str, new_content: &str) -> String {
    let (frontmatter, body) = extract_frontmatter(content);
    match frontmatter {
        Some(fm) => format!("---\n{fm}\n---\n{new_content}\n{body}"),
        None => format!("{new_content}\n{content}"),
    }
}

/// Parse a heading string like `"### Q2 Goals"` into
/// `(level, "Q2 Goals")`. Returns `None` if the string isn't a valid
/// ATX heading (1..=6 `#`s followed by a space and at least one
/// non-whitespace character).
fn parse_heading_string(s: &str) -> Option<(u8, String)> {
    let trimmed = s.trim();
    let level = trimmed.chars().take_while(|c| *c == '#').count() as u8;
    if level == 0 || level > 6 {
        return None;
    }
    let after = &trimmed[level as usize..];
    if !after.starts_with(' ') {
        return None;
    }
    let text = after.trim_start().to_string();
    if text.is_empty() {
        return None;
    }
    Some((level, text))
}

/// Extract the document title — the first H1, with inline formatting
/// flattened to plain text. Returns `None` if the document has no H1.
pub fn extract_title(content: &str) -> Option<String> {
    list_headings(content)
        .into_iter()
        .find(|h| h.level == 1)
        .map(|h| h.text)
}

/// Split `content` into `(frontmatter, body)`.
///
/// Frontmatter is the YAML block delimited by `---` fences at the very
/// top of the document. If no frontmatter is present, returns
/// `(None, content.to_string())`.
pub fn extract_frontmatter(content: &str) -> (Option<String>, String) {
    let mut lines = content.lines();
    let Some(first) = lines.next() else {
        return (None, String::new());
    };
    if first.trim() != "---" {
        return (None, content.to_string());
    }
    // Find the closing `---` line.
    let mut frontmatter_lines: Vec<&str> = Vec::new();
    let mut closing_found = false;
    let mut consumed = first.len() + 1; // +1 for the newline
    for line in lines.by_ref() {
        consumed += line.len() + 1;
        if line.trim() == "---" {
            closing_found = true;
            break;
        }
        frontmatter_lines.push(line);
    }
    if !closing_found {
        return (None, content.to_string());
    }
    let frontmatter = frontmatter_lines.join("\n");
    let body_start = consumed.min(content.len());
    let body = content[body_start..].to_string();
    (Some(frontmatter), body)
}

/// Concatenate every text-like child of a heading into a flat string.
fn heading_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut out = String::new();
    for child in node.descendants() {
        let data = child.data.borrow();
        match &data.value {
            NodeValue::Text(t) => out.push_str(t),
            NodeValue::Code(c) => out.push_str(&c.literal),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_headings_returns_levels_text_and_lines() {
        let md = "# Top\n\n## Errors\n\nbody\n\n### Sub\n";
        let headings = list_headings(md);
        assert_eq!(headings.len(), 3);

        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[0].text, "Top");
        assert_eq!(headings[0].raw_line, "# Top");
        assert_eq!(headings[0].line, 1);

        assert_eq!(headings[1].level, 2);
        assert_eq!(headings[1].text, "Errors");
        assert_eq!(headings[1].raw_line, "## Errors");
        assert_eq!(headings[1].line, 3);

        assert_eq!(headings[2].level, 3);
        assert_eq!(headings[2].text, "Sub");
        assert_eq!(headings[2].raw_line, "### Sub");
        assert_eq!(headings[2].line, 7);
    }

    // ─── Swift fixture port: MarkdownSectionsTests ──────────────────

    const SAMPLE_DOCUMENT: &str = "\
# Roadmap

## 2026

### Q1 Goals
- Ship editor
- Add sync

### Q2 Goals
- Launch CLI
- Add search

### Q3 Goals
- Templates";

    const FRONTMATTER_DOCUMENT: &str = "\
---
title: Roadmap
author: John
---

# Roadmap

## Goals
- Ship it";

    #[test]
    fn list_headings_finds_all_headings_swift_fixture() {
        let headings = list_headings(SAMPLE_DOCUMENT);
        assert_eq!(headings.len(), 5);
        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[0].raw_line, "# Roadmap");
        assert_eq!(headings[1].level, 2);
        assert_eq!(headings[1].raw_line, "## 2026");
        assert_eq!(headings[2].level, 3);
        assert_eq!(headings[2].raw_line, "### Q1 Goals");
        assert_eq!(headings[3].level, 3);
        assert_eq!(headings[3].raw_line, "### Q2 Goals");
        assert_eq!(headings[4].level, 3);
        assert_eq!(headings[4].raw_line, "### Q3 Goals");
    }

    #[test]
    fn list_headings_skips_frontmatter_swift_fixture() {
        let headings = list_headings(FRONTMATTER_DOCUMENT);
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].raw_line, "# Roadmap");
        assert_eq!(headings[1].raw_line, "## Goals");
    }

    #[test]
    fn list_headings_handles_no_headings_swift_fixture() {
        assert!(list_headings("Just some text\nwith no headings.").is_empty());
    }

    #[test]
    fn find_section_finds_q2_swift_fixture() {
        let section = find_section(SAMPLE_DOCUMENT, "### Q2 Goals").expect("find");
        assert_eq!(section.heading.text, "Q2 Goals");
        assert_eq!(section.heading.level, 3);
    }

    #[test]
    fn find_section_throws_for_missing_swift_fixture() {
        let err = find_section(SAMPLE_DOCUMENT, "### Q4 Goals").expect_err("missing");
        assert!(matches!(err, crate::markdown::MarkdownError::SectionNotFound(_)));
    }

    #[test]
    fn find_section_errors_on_ambiguous() {
        let doc = "## H\n\nbody A\n\n## H\n\nbody B\n";
        let err = find_section(doc, "## H").expect_err("ambiguous");
        match err {
            crate::markdown::MarkdownError::AmbiguousSection { count, .. } => assert_eq!(count, 2),
            other => panic!("expected AmbiguousSection, got {other:?}"),
        }
    }

    #[test]
    fn read_section_extracts_q2_swift_fixture() {
        let section = read_section(SAMPLE_DOCUMENT, "### Q2 Goals").expect("read");
        assert!(section.contains("### Q2 Goals"), "got:\n{section}");
        assert!(section.contains("- Launch CLI"), "got:\n{section}");
        assert!(section.contains("- Add search"), "got:\n{section}");
        assert!(!section.contains("### Q1 Goals"));
        assert!(!section.contains("### Q3 Goals"));
    }

    #[test]
    fn read_section_extracts_last_section_swift_fixture() {
        let section = read_section(SAMPLE_DOCUMENT, "### Q3 Goals").expect("read");
        assert!(section.contains("### Q3 Goals"));
        assert!(section.contains("- Templates"));
    }

    #[test]
    fn write_section_replaces_body_swift_fixture() {
        let updated = write_section(SAMPLE_DOCUMENT, "### Q2 Goals", "- New plan").expect("write");
        assert!(updated.contains("### Q2 Goals"));
        assert!(updated.contains("- New plan"));
        assert!(!updated.contains("- Launch CLI"));
        assert!(!updated.contains("- Add search"));
        // Other sections untouched.
        assert!(updated.contains("### Q1 Goals"));
        assert!(updated.contains("- Ship editor"));
        assert!(updated.contains("### Q3 Goals"));
        assert!(updated.contains("- Templates"));
    }

    #[test]
    fn append_to_section_inserts_at_end_swift_fixture() {
        let updated =
            append_to_section(SAMPLE_DOCUMENT, "### Q2 Goals", "- Mobile app").expect("append");
        assert!(updated.contains("- Add search"));
        assert!(updated.contains("- Mobile app"));
        let mobile_pos = updated.find("- Mobile app").expect("mobile present");
        let q3_pos = updated.find("### Q3 Goals").expect("q3 present");
        assert!(mobile_pos < q3_pos, "Mobile app should land before Q3");
    }

    #[test]
    fn prepend_to_section_inserts_after_heading_swift_fixture() {
        let updated = prepend_to_section(SAMPLE_DOCUMENT, "### Q2 Goals", "> Priority: High")
            .expect("prepend");
        assert!(updated.contains("> Priority: High"));
        let heading_pos = updated.find("### Q2 Goals").expect("heading");
        let prio_pos = updated.find("> Priority: High").expect("priority");
        let content_pos = updated.find("- Launch CLI").expect("content");
        assert!(heading_pos < prio_pos);
        assert!(prio_pos < content_pos);
    }

    #[test]
    fn remove_section_deletes_q2_swift_fixture() {
        let updated = remove_section(SAMPLE_DOCUMENT, "### Q2 Goals").expect("remove");
        assert!(!updated.contains("### Q2 Goals"));
        assert!(!updated.contains("- Launch CLI"));
        assert!(!updated.contains("- Add search"));
        assert!(updated.contains("### Q1 Goals"));
        assert!(updated.contains("### Q3 Goals"));
    }

    #[test]
    fn extract_frontmatter_splits_correctly_swift_fixture() {
        let (fm, body) = extract_frontmatter(FRONTMATTER_DOCUMENT);
        assert!(fm.is_some());
        let fm = fm.unwrap();
        assert!(fm.contains("title: Roadmap"));
        assert!(fm.contains("author: John"));
        assert!(body.contains("# Roadmap"));
    }

    #[test]
    fn extract_frontmatter_handles_no_frontmatter_swift_fixture() {
        let input = "# Just a heading\nSome content";
        let (fm, body) = extract_frontmatter(input);
        assert_eq!(fm, None);
        assert_eq!(body, input);
    }

    #[test]
    fn prepend_to_document_inserts_after_frontmatter_swift_fixture() {
        let result = prepend_to_document(FRONTMATTER_DOCUMENT, "> Status: Draft");
        assert!(result.contains("---"));
        assert!(result.contains("> Status: Draft"));
        // Banner lands after the closing `---` fence.
        let first_fence_end = result.find("---").unwrap() + 3;
        let second_fence_pos = result[first_fence_end..]
            .find("---")
            .map(|p| first_fence_end + p)
            .expect("closing fence");
        let status_pos = result.find("> Status: Draft").expect("status");
        assert!(second_fence_pos < status_pos);
    }

    #[test]
    fn prepend_to_document_inserts_at_top_without_frontmatter_swift_fixture() {
        let result = prepend_to_document("# Heading\nContent", "> Banner");
        assert!(result.starts_with("> Banner"));
    }

    #[test]
    fn list_headings_skips_headings_inside_fenced_code_blocks() {
        let md = "# Real\n\n```\n## Not a heading\n```\n\n## Also real\n";
        let headings = list_headings(md);
        let texts: Vec<&str> = headings.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(texts, vec!["Real", "Also real"]);
    }

    #[test]
    fn list_headings_flattens_inline_formatting_to_plain_text() {
        let md = "# Hello *world* `now`\n";
        let headings = list_headings(md);
        assert_eq!(headings[0].text, "Hello world now");
    }

    #[test]
    fn list_headings_empty_document_returns_empty_vec() {
        assert!(list_headings("").is_empty());
    }

    #[test]
    fn extract_title_returns_first_h1() {
        assert_eq!(
            extract_title("# Hello\n\n## Sub\n"),
            Some("Hello".to_string())
        );
    }

    #[test]
    fn extract_title_returns_none_when_no_h1() {
        assert_eq!(extract_title("## Only H2 here\n"), None);
        assert_eq!(extract_title(""), None);
    }

    #[test]
    fn extract_title_picks_first_h1_even_with_h2s_above_being_invalid() {
        // First H1 wins regardless of any other heading levels in front
        // (markdown allows multiple H1s; we just take the first).
        assert_eq!(
            extract_title("## Skipped\n\n# Title\n\n# Second title\n"),
            Some("Title".to_string())
        );
    }

    #[test]
    fn extract_frontmatter_splits_on_fence() {
        let md = "---\nname: Planning\nversion: 1\n---\n\n# Body starts here\n";
        let (fm, body) = extract_frontmatter(md);
        assert_eq!(fm.as_deref(), Some("name: Planning\nversion: 1"));
        assert_eq!(body, "\n# Body starts here\n");
    }

    #[test]
    fn extract_frontmatter_returns_none_when_no_fence() {
        let md = "# No frontmatter here\n\nBody.\n";
        let (fm, body) = extract_frontmatter(md);
        assert_eq!(fm, None);
        assert_eq!(body, md);
    }

    #[test]
    fn extract_frontmatter_returns_none_when_opening_fence_is_unclosed() {
        let md = "---\nname: Planning\n\n# No closing fence\n";
        let (fm, body) = extract_frontmatter(md);
        assert_eq!(fm, None);
        assert_eq!(body, md);
    }
}
