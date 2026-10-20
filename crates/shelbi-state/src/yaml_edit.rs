//! Comment-preserving edits to YAML config files.
//!
//! The same approach as [`keymap::edit`](crate::keymap::edit), generalized
//! from one mapping entry to a whole document. A caller says what the file
//! should *mean* (a [`Value`]); [`patch`] works out the text. Every edit is
//! made twice: structurally, as the target tree, then on the text, so the
//! header, comments, blank lines, key order, quoting and flow style of
//! everything the edit didn't touch survive. The text is kept only when it
//! parses back to exactly the target; otherwise a coarser text edit is
//! tried, and only as a last resort is the tree re-serialized (correct, but
//! comments are lost; [`Patched::dropped_comments`] says so).
//!
//! The text editor walks block mappings and block sequences. Unchanged
//! entries and items keep their lines verbatim (with the comments above
//! them); a changed entry is patched inside when both sides are block
//! collections, rewritten in place when it's a one-line flow collection
//! (`actions: [push_branch, open_pr]` stays flow), and re-emitted through
//! serde otherwise. Sequence items are matched by identity (`id:`, or
//! `from:` + `to:`, else the value), so a reordered list moves each item's
//! text, comments included.
//!
//! [`merge`] builds the target tree for a typed edit: it overlays only the
//! differences between the typed value as loaded and as edited onto the raw
//! tree, so keys the typed model doesn't know (or spells differently)
//! survive.

use serde_yaml::{Mapping, Value};

/// Why a file couldn't be patched.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    /// The file doesn't parse as YAML. It is left alone.
    #[error("doesn't parse as YAML: {0}")]
    Parse(String),
    #[error("couldn't serialize: {0}")]
    Serialize(String),
}

/// A patched file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patched {
    pub text: String,
    /// Comment lines of the old text that the new one no longer has. Empty
    /// unless the edit removed what a comment sat on, or fell back to
    /// re-serializing a block.
    pub dropped_comments: Vec<String>,
}

/// The file's YAML tree; an empty or comment-only file is an empty mapping.
pub fn parse(text: &str) -> Result<Value, PatchError> {
    match serde_yaml::from_str::<Value>(text) {
        Ok(Value::Null) => Ok(Value::Mapping(Mapping::new())),
        Ok(v) => Ok(v),
        Err(e) => Err(PatchError::Parse(e.to_string())),
    }
}

/// Rewrite `text` so it parses to `new`, changing as little text as it can.
pub fn patch(text: &str, new: &Value) -> Result<Patched, PatchError> {
    let old = parse(text)?;
    if old == *new {
        return Ok(Patched {
            text: text.to_string(),
            dropped_comments: Vec::new(),
        });
    }
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    for depth in [Depth::Fine, Depth::Coarse] {
        let Some(out) = patch_block(&lines, &old, new, depth, true) else {
            continue;
        };
        let out = join(&out);
        if parse(&out).is_ok_and(|v| v == *new) {
            return Ok(finish(text, out));
        }
    }
    let out = if new.as_mapping().is_some_and(Mapping::is_empty) {
        String::new()
    } else {
        serde_yaml::to_string(new).map_err(|e| PatchError::Serialize(e.to_string()))?
    };
    Ok(finish(text, out))
}

/// The tree to write for a typed edit: `raw` (the file as parsed) with the
/// differences between `base` (the typed value as loaded, serialized) and
/// `new` (as edited, serialized) applied. What `base` and `new` agree on is
/// left as `raw` has it, so unknown keys, legacy spellings and values the
/// typed form normalizes stay put. `aliases` pairs a canonical key with an
/// alternate spelling the file may use instead (`tags` / `tag`); writing
/// the canonical key replaces the alternate.
pub fn merge(raw: &Value, base: &Value, new: &Value, aliases: &[(&str, &str)]) -> Value {
    if base == new {
        return raw.clone();
    }
    match (raw, base, new) {
        (Value::Mapping(r), Value::Mapping(b), Value::Mapping(n)) => {
            let mut out = r.clone();
            for k in b.keys() {
                if !n.contains_key(k) {
                    out.shift_remove(k);
                    if let Some(alias) = alias_of(k, aliases) {
                        out.shift_remove(alias);
                    }
                }
            }
            for (k, nv) in n {
                let bv = b.get(k);
                if bv == Some(nv) {
                    continue;
                }
                let alias =
                    alias_of(k, aliases).filter(|a| !out.contains_key(k) && out.contains_key(*a));
                let rv = match alias {
                    Some(a) => out.shift_remove(a),
                    None => out.get(k).cloned(),
                };
                let merged = match (rv, bv) {
                    (Some(rv), Some(bv)) => merge(&rv, bv, nv, aliases),
                    _ => nv.clone(),
                };
                out.insert(k.clone(), merged);
            }
            Value::Mapping(out)
        }
        (Value::Sequence(r), Value::Sequence(b), Value::Sequence(n)) if r.len() == b.len() => {
            let pairs = match_items(b, n, false);
            let items = n
                .iter()
                .zip(pairs)
                .map(|(nv, j)| match j {
                    Some(j) => merge(&r[j], &b[j], nv, aliases),
                    None => nv.clone(),
                })
                .collect();
            Value::Sequence(items)
        }
        _ => new.clone(),
    }
}

fn alias_of<'a>(key: &Value, aliases: &[(&str, &'a str)]) -> Option<&'a str> {
    let key = key.as_str()?;
    aliases.iter().find(|(c, _)| *c == key).map(|(_, a)| *a)
}

/// What a sequence item is matched by across an edit: a mapping's `id`, or
/// its `from` + `to`, else the whole value.
fn identity(v: &Value) -> Value {
    if let Value::Mapping(m) = v {
        if let Some(id) = m.get("id") {
            return Value::Sequence(vec!["id".into(), id.clone()]);
        }
        if let (Some(from), Some(to)) = (m.get("from"), m.get("to")) {
            return Value::Sequence(vec!["edge".into(), from.clone(), to.clone()]);
        }
    }
    v.clone()
}

/// For each item of `new`, the item of `old` it continues: the first unused
/// one with the same [`identity`], else (when `positional`) the unused one
/// at the same index.
fn match_items(old: &[Value], new: &[Value], positional: bool) -> Vec<Option<usize>> {
    let old_ids: Vec<Value> = old.iter().map(identity).collect();
    let mut used = vec![false; old.len()];
    let mut pairs: Vec<Option<usize>> = new
        .iter()
        .map(|nv| {
            let id = identity(nv);
            let j = (0..old.len()).find(|&j| !used[j] && old_ids[j] == id)?;
            used[j] = true;
            Some(j)
        })
        .collect();
    if positional {
        for (i, pair) in pairs.iter_mut().enumerate() {
            if pair.is_none() && i < old.len() && !used[i] {
                used[i] = true;
                *pair = Some(i);
            }
        }
    }
    pairs
}

/// How far the text editor follows a changed value before re-emitting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    /// Into nested block collections.
    Fine,
    /// Re-emit each changed top-level entry or item whole.
    Coarse,
}

/// `top` is the document itself: its leading lines are the file's header,
/// which stays put. In a nested block they're the first entry's comments
/// and move with it.
fn patch_block(
    lines: &[String],
    old: &Value,
    new: &Value,
    depth: Depth,
    top: bool,
) -> Option<Vec<String>> {
    match (old, new) {
        (Value::Mapping(o), Value::Mapping(n)) => patch_map(lines, o, n, depth, top),
        (Value::Sequence(o), Value::Sequence(n)) => patch_seq(lines, o, n, depth, top),
        _ => None,
    }
}

/// A block collection's lines, split into its entries (or items).
struct Block<'a> {
    /// Lines before the first entry.
    preamble: &'a [String],
    parts: Vec<Part<'a>>,
    /// Blank and comment lines after the last entry.
    postamble: &'a [String],
    indent: usize,
}

struct Part<'a> {
    /// Blank and comment lines between the previous entry and this one.
    leading: &'a [String],
    /// The entry itself, from its key (or `-`) line.
    span: &'a [String],
}

/// Split block-collection lines into entries at their first content line's
/// indentation. `None` for anything it doesn't follow.
fn split(lines: &[String], seq: bool, top: bool) -> Option<Block<'_>> {
    let Some(first) = lines.iter().position(|l| is_content(l)) else {
        return Some(Block {
            preamble: lines,
            parts: Vec::new(),
            postamble: &[],
            indent: 0,
        });
    };
    let ind = indent(&lines[first]);
    let mut parts = Vec::new();
    let mut at = first;
    let preamble_end = if top { first } else { 0 };
    let mut leading_from = preamble_end;
    loop {
        let line = &lines[at];
        let opens = if seq {
            if !is_item(line) {
                return None;
            }
            false
        } else {
            let (_, rest) = key_of(line)?;
            let rest = rest.trim();
            rest.is_empty() || rest.starts_with('#')
        };
        let end = span_end(lines, at, ind, opens);
        parts.push(Part {
            leading: &lines[leading_from..at],
            span: &lines[at..end],
        });
        match (end..lines.len()).find(|&j| is_content(&lines[j])) {
            None => {
                return Some(Block {
                    preamble: &lines[..preamble_end],
                    parts,
                    postamble: &lines[end..],
                    indent: ind,
                })
            }
            Some(next) => {
                if indent(&lines[next]) != ind {
                    return None;
                }
                leading_from = end;
                at = next;
            }
        }
    }
}

/// One past the last line of the entry at `at`: its deeper lines (and, for
/// a key that opens a block, a block list at its own indentation), without
/// trailing blank or comment lines.
fn span_end(lines: &[String], at: usize, ind: usize, opens: bool) -> usize {
    let mut end = at + 1;
    for (j, line) in lines.iter().enumerate().skip(at + 1) {
        if !is_content(line) {
            continue;
        }
        let li = indent(line);
        if li > ind || (opens && li == ind && is_item(line)) {
            end = j + 1;
        } else {
            break;
        }
    }
    end
}

fn patch_map(
    lines: &[String],
    old: &Mapping,
    new: &Mapping,
    depth: Depth,
    top: bool,
) -> Option<Vec<String>> {
    let block = split(lines, false, top)?;
    let keys: Vec<String> = block
        .parts
        .iter()
        .map(|p| key_of(&p.span[0]).map(|(k, _)| k))
        .collect::<Option<_>>()?;
    let old_keys: Vec<&str> = old.keys().map(Value::as_str).collect::<Option<_>>()?;
    if keys != old_keys {
        return None;
    }
    let mut out = block.preamble.to_vec();
    for (k, nv) in new {
        let k = k.as_str()?;
        let Some(p) = keys.iter().position(|x| x == k) else {
            out.extend(emit_entry(block.indent, k, nv, None)?);
            continue;
        };
        let part = &block.parts[p];
        out.extend_from_slice(part.leading);
        let ov = old.get(k)?;
        if ov == nv {
            out.extend_from_slice(part.span);
            continue;
        }
        match patch_entry(part.span, ov, nv, depth) {
            Some(lines) => out.extend(lines),
            None => out.extend(emit_entry(
                block.indent,
                k,
                nv,
                one_line_comment(part.span),
            )?),
        }
    }
    out.extend_from_slice(block.postamble);
    Some(out)
}

/// Patch one changed mapping entry in place, or `None` to re-emit it.
fn patch_entry(span: &[String], ov: &Value, nv: &Value, depth: Depth) -> Option<Vec<String>> {
    let first = &span[0];
    let (_, rest) = key_of(first)?;
    let value = rest.trim();
    if value.is_empty() || value.starts_with('#') {
        if depth == Depth::Coarse || collection_is_empty(nv) {
            return None;
        }
        let mut out = vec![first.clone()];
        out.extend(patch_block(&span[1..], ov, nv, depth, false)?);
        return Some(out);
    }
    if span.len() == 1 && value.starts_with(['[', '{']) {
        let flow = flow(nv, padded(value))?;
        let head = &first[..first.len() - rest.len()];
        return Some(vec![with_comment(
            format!("{head} {flow}"),
            comment_tail(first),
        )]);
    }
    None
}

fn patch_seq(
    lines: &[String],
    old: &[Value],
    new: &[Value],
    depth: Depth,
    top: bool,
) -> Option<Vec<String>> {
    let block = split(lines, true, top)?;
    if block.parts.len() != old.len() {
        return None;
    }
    let ind = block.indent;
    // New items follow their siblings' style: flow when they're flow.
    let style = block.parts.iter().find_map(|p| {
        let item = p.span[0][ind + 1..].trim();
        (p.span.len() == 1 && item.starts_with('{')).then(|| padded(item))
    });
    let pairs = match_items(old, new, true);
    let mut out = block.preamble.to_vec();
    for (nv, j) in new.iter().zip(pairs) {
        let Some(j) = j else {
            out.extend(emit_item(ind, nv, style, None)?);
            continue;
        };
        let part = &block.parts[j];
        out.extend_from_slice(part.leading);
        if old[j] == *nv {
            out.extend_from_slice(part.span);
            continue;
        }
        match patch_item(part.span, ind, &old[j], nv, depth) {
            Some(lines) => out.extend(lines),
            None => out.extend(emit_item(ind, nv, style, one_line_comment(part.span))?),
        }
    }
    out.extend_from_slice(block.postamble);
    Some(out)
}

/// Patch one changed sequence item in place, or `None` to re-emit it.
fn patch_item(
    span: &[String],
    ind: usize,
    ov: &Value,
    nv: &Value,
    depth: Depth,
) -> Option<Vec<String>> {
    let first = &span[0];
    let item = first[ind + 1..].trim();
    if item.is_empty() || item.starts_with('#') {
        if depth == Depth::Coarse || collection_is_empty(nv) {
            return None;
        }
        let mut out = vec![first.clone()];
        out.extend(patch_block(&span[1..], ov, nv, depth, false)?);
        return Some(out);
    }
    if span.len() == 1 && item.starts_with(['[', '{']) {
        let flow = flow(nv, padded(item))?;
        let line = format!("{}- {flow}", " ".repeat(ind));
        return Some(vec![with_comment(line, comment_tail(first))]);
    }
    if depth == Depth::Coarse || collection_is_empty(nv) {
        return None;
    }
    // `- id: x` starts a mapping on the dash line: patch it as a mapping
    // with the dash blanked, then put the dash back on its first line.
    let (Value::Mapping(o), Value::Mapping(n)) = (ov, nv) else {
        return None;
    };
    key_of(item)?;
    let mut lines = span.to_vec();
    lines[0].replace_range(ind..ind + 1, " ");
    let mut out = patch_map(&lines, o, n, depth, false)?;
    let at = out.iter().position(|l| is_content(l))?;
    if indent(&out[at]) <= ind {
        return None;
    }
    out[at].replace_range(ind..ind + 1, "-");
    Some(out)
}

fn collection_is_empty(v: &Value) -> bool {
    match v {
        Value::Mapping(m) => m.is_empty(),
        Value::Sequence(s) => s.is_empty(),
        _ => true,
    }
}

/// `key: value` as serde writes it, at `ind`.
fn emit_entry(ind: usize, key: &str, v: &Value, comment: Option<&str>) -> Option<Vec<String>> {
    let mut m = Mapping::new();
    m.insert(Value::String(key.to_string()), v.clone());
    emit(ind, &Value::Mapping(m), comment)
}

/// `- value` at `ind`: flow when its siblings are, else as serde writes it.
fn emit_item(
    ind: usize,
    v: &Value,
    style: Option<bool>,
    comment: Option<&str>,
) -> Option<Vec<String>> {
    if let Some(flow) = style.and_then(|padded| flow(v, padded)) {
        return Some(vec![with_comment(
            format!("{}- {flow}", " ".repeat(ind)),
            comment,
        )]);
    }
    emit(ind, &Value::Sequence(vec![v.clone()]), comment)
}

fn emit(ind: usize, v: &Value, comment: Option<&str>) -> Option<Vec<String>> {
    let text = serde_yaml::to_string(v).ok()?;
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{}{l}", " ".repeat(ind))
            }
        })
        .collect();
    if let [line] = lines.as_mut_slice() {
        *line = with_comment(std::mem::take(line), comment);
    }
    Some(lines)
}

/// `line` with a comment tail ([`comment_tail`]) put back.
fn with_comment(mut line: String, tail: Option<&str>) -> String {
    if let Some(tail) = tail {
        line.push_str(tail);
    }
    line
}

/// The trailing comment of a one-line entry or item, as [`comment_tail`].
fn one_line_comment(span: &[String]) -> Option<&str> {
    match span {
        [line] => comment_tail(line),
        _ => None,
    }
}

/// A line's trailing comment with the whitespace before it, so a rewritten
/// value keeps the comment's column gap.
fn comment_tail(line: &str) -> Option<&str> {
    let comment = trailing_comment(line)?;
    let start = line.len() - comment.len();
    let gap = line[..start].len() - line[..start].trim_end().len();
    Some(&line[start - gap..])
}

/// Whether a flow collection is written with inner padding (`{ a: 1 }`).
fn padded(flow: &str) -> bool {
    flow.get(1..2) == Some(" ")
}

/// `v` as a one-line flow value, or `None` when it has no faithful one.
fn flow(v: &Value, padded: bool) -> Option<String> {
    Some(match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => flow_scalar(s)?,
        Value::Sequence(items) => {
            let items: Vec<String> = items
                .iter()
                .map(|i| flow(i, padded))
                .collect::<Option<_>>()?;
            format!("[{}]", items.join(", "))
        }
        Value::Mapping(m) if m.is_empty() => "{}".to_string(),
        Value::Mapping(m) => {
            let entries: Vec<String> = m
                .iter()
                .map(|(k, v)| {
                    Some(format!(
                        "{}: {}",
                        flow_scalar(k.as_str()?)?,
                        flow(v, padded)?
                    ))
                })
                .collect::<Option<_>>()?;
            if padded {
                format!("{{ {} }}", entries.join(", "))
            } else {
                format!("{{{}}}", entries.join(", "))
            }
        }
        Value::Tagged(_) => return None,
    })
}

/// A string as a flow scalar: plain when it can only read as that string,
/// else single-quoted.
fn flow_scalar(s: &str) -> Option<String> {
    if s.contains(['\n', '\r', '\t']) {
        return None;
    }
    let plain = !s.is_empty()
        && s.trim() == s
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || " -_./$+=@()".contains(c))
        && serde_yaml::from_str::<Value>(s).is_ok_and(|v| v == Value::String(s.to_string()));
    Some(if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "''"))
    })
}

fn join(lines: &[String]) -> String {
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// The patch result, with the comment lines it lost.
fn finish(old: &str, new: String) -> Patched {
    let mut kept: Vec<&str> = new.lines().filter_map(comment_text).collect();
    let mut dropped = Vec::new();
    for c in old.lines().filter_map(comment_text) {
        match kept.iter().position(|k| *k == c) {
            Some(i) => {
                kept.swap_remove(i);
            }
            None => dropped.push(c.to_string()),
        }
    }
    Patched {
        text: new,
        dropped_comments: dropped,
    }
}

/// A comment, as the text from its `#`: a whole comment line or one ending
/// an entry.
fn comment_text(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.starts_with('#') {
        return Some(t.trim_end());
    }
    trailing_comment(line).map(str::trim_end)
}

pub(crate) fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

pub(crate) fn is_content(line: &str) -> bool {
    let t = line.trim_start();
    !t.is_empty() && !t.starts_with('#')
}

/// A block sequence item line: `-` then a space or the end of the line.
fn is_item(line: &str) -> bool {
    let t = line.trim_start();
    t == "-" || t.starts_with("- ")
}

/// A mapping entry's key (unquoted) and what follows its colon.
pub(crate) fn key_of(line: &str) -> Option<(String, &str)> {
    let t = line.trim_start();
    if let Some(q @ ('"' | '\'')) = t.chars().next() {
        let close = t[1..].find(q)? + 1;
        let rest = t[close + 1..].strip_prefix(':')?;
        if !(rest.is_empty() || rest.starts_with(' ')) {
            return None;
        }
        return Some((t[1..close].to_string(), rest));
    }
    if t.starts_with(['-', '{', '[', '?', '&', '*', '!', '|', '>', '#']) {
        return None;
    }
    let mut search = 0;
    loop {
        let colon = search + t[search..].find(':')?;
        let rest = &t[colon + 1..];
        if rest.is_empty() || rest.starts_with(' ') {
            return Some((t[..colon].trim_end().to_string(), rest));
        }
        search = colon + 1;
    }
}

/// The `# ...` comment ending a content line, if any: a `#` after
/// whitespace, outside quotes (a quote only opens at the start of a token).
pub(crate) fn trailing_comment(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if !is_content(line) {
        return None;
    }
    let start = line.len() - t.len();
    let mut quote = None;
    let mut prev = ' ';
    for (i, c) in t.char_indices() {
        match (quote, c) {
            (None, '\'' | '"') if prev.is_whitespace() || "[{,:-".contains(prev) => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '#') if prev.is_whitespace() => return Some(&line[start + i..]),
            _ => {}
        }
        prev = c;
    }
    None
}

#[cfg(test)]
mod tests;
