//! Per-awareness-mode variants inside agent instructions.
//!
//! The orchestrator's instructions teach one of two event systems, chosen by
//! the project's `orchestrator.awareness` setting: the heartbeat (v1, a
//! periodic `heartbeat` line, a Monitor on `events tail` and a self-driven
//! claim/ack drain) or the awareness system (v2, events delivered on their
//! own, reminders, watched jobs and task estimates). The passages that differ
//! sit in marked blocks, one line per marker:
//!
//! ```text
//! <!-- shelbi:heartbeat -->
//! ...read only by a heartbeat (v1) orchestrator...
//! <!-- /shelbi:heartbeat -->
//! <!-- shelbi:awareness -->
//! ...read only by an awareness (v2) orchestrator...
//! <!-- /shelbi:awareness -->
//! ```
//!
//! [`select_awareness_variant`] keeps the blocks of one mode, drops the
//! other's, and removes every marker line, so the composed instructions carry
//! no trace of the markup. Text outside any block is shared by both modes and
//! passes through byte for byte. An open marker may carry a note after the
//! mode name (`<!-- shelbi:awareness v2 only -->`).
//!
//! Malformed markup degrades rather than fails: a block left open runs to the
//! end of the file, a stray close marker is dropped, and a marker inside a
//! block other than its own close is treated as that block's content.

use std::ops::Range;

use shelbi_core::AwarenessMode;

/// The prefix every marker's name starts with (`shelbi:heartbeat`,
/// `/shelbi:awareness`).
const MARKER_NAMESPACE: &str = "shelbi:";

/// What a single line is, as far as the variant markup goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Open(AwarenessMode),
    Close(AwarenessMode),
}

/// Parse one line as a marker. Only a line that is nothing but an HTML
/// comment naming a mode counts; anything else is content.
fn parse_marker(line: &str) -> Option<Marker> {
    let inner = line
        .trim()
        .strip_prefix("<!--")?
        .strip_suffix("-->")?
        .trim();
    if let Some(name) = inner.strip_prefix('/') {
        let mode = AwarenessMode::parse(name.trim().strip_prefix(MARKER_NAMESPACE)?)?;
        return Some(Marker::Close(mode));
    }
    let rest = inner.strip_prefix(MARKER_NAMESPACE)?;
    let name = rest.split_whitespace().next()?;
    AwarenessMode::parse(name).map(Marker::Open)
}

/// The open marker line for `mode`, as the shipped template writes it.
pub fn awareness_open_marker(mode: AwarenessMode) -> String {
    format!("<!-- {MARKER_NAMESPACE}{} -->", mode.as_str())
}

/// The close marker line for `mode`, as the shipped template writes it.
pub fn awareness_close_marker(mode: AwarenessMode) -> String {
    format!("<!-- /{MARKER_NAMESPACE}{} -->", mode.as_str())
}

/// One marked block: its mode, the byte span of the whole block (open marker
/// through close marker, inclusive of the close line's newline), and the span
/// of its content between the markers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Block {
    mode: AwarenessMode,
    span: Range<usize>,
    content: Range<usize>,
}

/// Scan `text` into its marked blocks, in order, plus the spans of stray
/// marker lines (a close with no open block).
fn scan(text: &str) -> (Vec<Block>, Vec<Range<usize>>) {
    let mut blocks = Vec::new();
    let mut strays = Vec::new();
    // The open block: its mode, where its open line starts, where its content
    // starts.
    let mut open: Option<(AwarenessMode, usize, usize)> = None;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let marker = parse_marker(line);
        match (open, marker) {
            (None, Some(Marker::Open(mode))) => open = Some((mode, start, at)),
            (None, Some(Marker::Close(_))) => strays.push(start..at),
            (Some((mode, from, content)), Some(Marker::Close(closing))) if closing == mode => {
                blocks.push(Block {
                    mode,
                    span: from..at,
                    content: content..start,
                });
                open = None;
            }
            _ => {}
        }
    }
    if let Some((mode, from, content)) = open {
        blocks.push(Block {
            mode,
            span: from..text.len(),
            content: content..text.len(),
        });
    }
    (blocks, strays)
}

/// The byte spans of every marked block in `text`, markers included, in order.
/// A config upgrade uses them to leave already-marked text alone.
pub fn awareness_marked_spans(text: &str) -> Vec<Range<usize>> {
    scan(text).0.into_iter().map(|b| b.span).collect()
}

/// Whether `text` carries any awareness variant markup.
pub fn has_awareness_variants(text: &str) -> bool {
    text.lines().any(|line| parse_marker(line).is_some())
}

/// Compose `text` for `mode`: keep the content of `mode`'s blocks, drop the
/// other mode's blocks whole, and remove every marker line. Text with no
/// markup is returned unchanged.
pub fn select_awareness_variant(text: &str, mode: AwarenessMode) -> String {
    let (blocks, strays) = scan(text);
    if blocks.is_empty() && strays.is_empty() {
        return text.to_string();
    }
    // Every span to cut, in order: a whole block of the other mode, or the
    // two marker lines around a block of this mode, or a stray marker.
    let mut cuts: Vec<Range<usize>> = strays;
    for block in &blocks {
        if block.mode == mode {
            cuts.push(block.span.start..block.content.start);
            cuts.push(block.content.end..block.span.end);
        } else {
            cuts.push(block.span.clone());
        }
    }
    cuts.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for cut in cuts {
        if cut.start > at {
            out.push_str(&text[at..cut.start]);
        }
        at = at.max(cut.end);
    }
    out.push_str(&text[at..]);
    out
}

/// A run of adjacent marked blocks — in the shipped template, a heartbeat
/// block immediately followed by its awareness counterpart. The unit the
/// config upgrade adds to an instructions file written before the markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantGroup<'a> {
    /// The whole group, markers included, exactly as it appears in the text.
    pub text: &'a str,
    /// What a heartbeat orchestrator reads from the group (the content of its
    /// heartbeat blocks; empty when the group has none).
    pub heartbeat: String,
    /// What an awareness orchestrator reads from the group.
    pub awareness: String,
}

/// The variant groups of `text`, in order. Blocks are grouped when one's
/// close marker line is directly followed by the next one's open marker line.
pub fn awareness_variant_groups(text: &str) -> Vec<VariantGroup<'_>> {
    let (blocks, _) = scan(text);
    let mut groups: Vec<Vec<&Block>> = Vec::new();
    for block in &blocks {
        match groups.last_mut() {
            Some(group) if group.last().is_some_and(|prev| prev.span.end == block.span.start) => {
                group.push(block)
            }
            _ => groups.push(vec![block]),
        }
    }
    groups
        .into_iter()
        .map(|group| {
            let start = group.first().map_or(0, |b| b.span.start);
            let end = group.last().map_or(0, |b| b.span.end);
            let read = |mode: AwarenessMode| -> String {
                group
                    .iter()
                    .filter(|b| b.mode == mode)
                    .map(|b| &text[b.content.clone()])
                    .collect()
            };
            VariantGroup {
                text: &text[start..end],
                heartbeat: read(AwarenessMode::Heartbeat),
                awareness: read(AwarenessMode::Awareness),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HB: AwarenessMode = AwarenessMode::Heartbeat;
    const AW: AwarenessMode = AwarenessMode::Awareness;

    const SAMPLE: &str = "\
intro

<!-- shelbi:heartbeat -->
v1 only
<!-- /shelbi:heartbeat -->
<!-- shelbi:awareness -->
v2 only
<!-- /shelbi:awareness -->

shared tail
";

    #[test]
    fn text_without_markup_passes_through() {
        let text = "# Title\n\n<!-- a comment -->\nbody";
        assert_eq!(select_awareness_variant(text, HB), text);
        assert_eq!(select_awareness_variant(text, AW), text);
        assert!(!has_awareness_variants(text));
    }

    #[test]
    fn each_mode_keeps_its_own_block_and_loses_the_markers() {
        assert_eq!(select_awareness_variant(SAMPLE, HB), "intro\n\nv1 only\n\nshared tail\n");
        assert_eq!(select_awareness_variant(SAMPLE, AW), "intro\n\nv2 only\n\nshared tail\n");
        assert!(has_awareness_variants(SAMPLE));
    }

    #[test]
    fn open_marker_may_carry_a_note_and_markers_tolerate_indentation() {
        let text = "a\n  <!--   shelbi:awareness  (v2 only) -->\nb\n<!--/shelbi:awareness-->\nc";
        assert_eq!(select_awareness_variant(text, AW), "a\nb\nc");
        assert_eq!(select_awareness_variant(text, HB), "a\nc");
    }

    #[test]
    fn unknown_names_are_ordinary_comments() {
        let text = "<!-- shelbi:other -->\nx\n<!-- shelbi:heartbeatish -->\n";
        assert_eq!(select_awareness_variant(text, AW), text);
    }

    #[test]
    fn unclosed_block_runs_to_the_end_and_stray_close_is_dropped() {
        let text = "a\n<!-- /shelbi:heartbeat -->\nb\n<!-- shelbi:heartbeat -->\nc\nd";
        assert_eq!(select_awareness_variant(text, HB), "a\nb\nc\nd");
        assert_eq!(select_awareness_variant(text, AW), "a\nb\n");
    }

    #[test]
    fn a_foreign_marker_inside_a_block_is_its_content() {
        let text = "<!-- shelbi:heartbeat -->\nx\n<!-- shelbi:awareness -->\ny\n<!-- /shelbi:heartbeat -->\nz\n";
        assert_eq!(
            select_awareness_variant(text, HB),
            "x\n<!-- shelbi:awareness -->\ny\nz\n"
        );
        assert_eq!(select_awareness_variant(text, AW), "z\n");
    }

    #[test]
    fn groups_pair_adjacent_blocks() {
        let text = format!("{SAMPLE}\n<!-- shelbi:awareness -->\nlone\n<!-- /shelbi:awareness -->\n");
        let groups = awareness_variant_groups(&text);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].heartbeat, "v1 only\n");
        assert_eq!(groups[0].awareness, "v2 only\n");
        assert!(groups[0].text.starts_with("<!-- shelbi:heartbeat -->\n"));
        assert!(groups[0].text.ends_with("<!-- /shelbi:awareness -->\n"));
        assert_eq!(groups[1].heartbeat, "");
        assert_eq!(groups[1].awareness, "lone\n");
    }

    #[test]
    fn marked_spans_cover_each_block_with_its_markers() {
        let spans = awareness_marked_spans(SAMPLE);
        assert_eq!(spans.len(), 2);
        assert!(SAMPLE[spans[0].clone()].starts_with("<!-- shelbi:heartbeat -->"));
        assert!(SAMPLE[spans[1].clone()].ends_with("<!-- /shelbi:awareness -->\n"));
        assert_eq!(spans[0].end, spans[1].start);
    }

    #[test]
    fn marker_helpers_round_trip_through_the_parser() {
        for mode in AwarenessMode::ALL {
            assert_eq!(parse_marker(&awareness_open_marker(mode)), Some(Marker::Open(mode)));
            assert_eq!(parse_marker(&awareness_close_marker(mode)), Some(Marker::Close(mode)));
        }
    }
}
