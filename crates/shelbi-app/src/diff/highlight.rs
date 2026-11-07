//! Syntax highlighting both clients paint from.
//!
//! The crate is [`syntect`], with bat's grammar bundle from [`two_face`] (it
//! adds TypeScript, TOML, Swift, Kotlin, Zig, Dockerfile and others that
//! syntect's defaults lack), built with the pure-Rust `fancy-regex` engine so
//! neither workspace gains a C dependency. It works a line at a time, which
//! suits hunks: a hunk is a fragment of a file that a tree-sitter grammar
//! would have to parse as a whole.
//!
//! Its colors aren't used. A token gets a [`SyntaxClass`] from its scope, and
//! each client maps classes onto its own theme, in light and dark.
//!
//! Each side of a hunk is parsed on its own, starting fresh at the hunk: old
//! is its context and deleted lines, new its context and added ones. A hunk
//! that starts inside a block comment reads wrong until the comment ends;
//! fetching whole files to avoid that isn't worth a round trip per file on a
//! remote worktree. This runs on the session's worker thread, a file at a
//! time, and a client paints plain text until a file's highlights arrive.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, ScopeStackOp, SyntaxReference, SyntaxSet};

use super::model::{FileDiff, LineKind};

/// A file with more hunk lines than this isn't highlighted.
pub const MAX_LINES: usize = 40_000;
/// A line longer than this isn't highlighted (minified code).
pub const MAX_LINE_LEN: usize = 4_096;

/// What a token is, for a client's theme to color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyntaxClass {
    Comment,
    String,
    Number,
    Constant,
    Keyword,
    Operator,
    Function,
    Type,
    Variable,
    Tag,
    Attribute,
    Heading,
    Punctuation,
}

/// A class over a byte range of a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxSpan {
    pub range: Range<u32>,
    pub class: SyntaxClass,
}

/// One file's highlights, by hunk and line. A line with no spans is plain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileHighlights {
    pub hunks: Vec<Vec<Vec<SyntaxSpan>>>,
}

impl FileHighlights {
    /// The spans of a line.
    pub fn line(&self, hunk: usize, line: usize) -> &[SyntaxSpan] {
        self.hunks.get(hunk).and_then(|h| h.get(line)).map_or(&[], Vec::as_slice)
    }
}

/// The grammar bundle, deserialized on first use. Tens of milliseconds, on the
/// worker thread.
fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// The grammar for `path`: by file name, then extension.
fn syntax_for(set: &'static SyntaxSet, path: &str) -> Option<&'static SyntaxReference> {
    let path = Path::new(path);
    let name = path.file_name()?.to_str()?;
    set.find_syntax_by_extension(name).or_else(|| {
        let ext = path.extension()?.to_str()?;
        set.find_syntax_by_extension(ext)
    })
}

/// The class a scope names, from its dotted name.
fn class_of(name: &str) -> Option<SyntaxClass> {
    // A string's quotes and a comment's `//` read as what they delimit.
    if name.starts_with("punctuation.definition") {
        return None;
    }
    const TABLE: &[(&str, SyntaxClass)] = &[
        ("comment", SyntaxClass::Comment),
        ("string", SyntaxClass::String),
        ("constant.numeric", SyntaxClass::Number),
        ("constant", SyntaxClass::Constant),
        ("keyword.operator", SyntaxClass::Operator),
        ("keyword", SyntaxClass::Keyword),
        ("storage", SyntaxClass::Keyword),
        ("entity.name.function", SyntaxClass::Function),
        ("support.function", SyntaxClass::Function),
        ("variable.function", SyntaxClass::Function),
        ("entity.name.tag", SyntaxClass::Tag),
        ("entity.other.attribute-name", SyntaxClass::Attribute),
        ("entity.name.section", SyntaxClass::Heading),
        ("markup.heading", SyntaxClass::Heading),
        ("entity.name", SyntaxClass::Type),
        ("support.type", SyntaxClass::Type),
        ("support.class", SyntaxClass::Type),
        ("variable.parameter", SyntaxClass::Variable),
        ("variable.language", SyntaxClass::Constant),
        ("variable", SyntaxClass::Variable),
        ("punctuation", SyntaxClass::Punctuation),
    ];
    TABLE
        .iter()
        .find(|(prefix, _)| name == *prefix || name.strip_prefix(prefix).is_some_and(|r| r.starts_with('.')))
        .map(|(_, class)| *class)
}

/// Runs a line through a grammar, reusing the scope-to-class lookups.
struct Classifier {
    cache: HashMap<Scope, Option<SyntaxClass>>,
}

impl Classifier {
    fn class(&mut self, scope: Scope) -> Option<SyntaxClass> {
        *self.cache.entry(scope).or_insert_with(|| class_of(&scope.build_string()))
    }

    /// The class of a stack: its innermost scope that names one.
    fn class_of_stack(&mut self, stack: &ScopeStack) -> Option<SyntaxClass> {
        stack.as_slice().iter().rev().find_map(|s| self.class(*s))
    }

    /// The spans of `text`, advancing `state`. `None` when the grammar fails
    /// on it: the rest of the hunk stays plain.
    fn line(
        &mut self,
        set: &SyntaxSet,
        state: &mut ParseState,
        stack: &mut ScopeStack,
        text: &str,
    ) -> Option<Vec<SyntaxSpan>> {
        let with_newline = format!("{text}\n");
        let ops = state.parse_line(&with_newline, set).ok()?;
        let mut spans: Vec<SyntaxSpan> = Vec::new();
        let mut at = 0usize;
        let mut push = |class: Option<SyntaxClass>, from: usize, to: usize| {
            let to = to.min(text.len());
            let Some(class) = class else { return };
            if from >= to {
                return;
            }
            match spans.last_mut() {
                Some(last) if last.class == class && last.range.end as usize == from => last.range.end = to as u32,
                _ => spans.push(SyntaxSpan { range: from as u32..to as u32, class }),
            }
        };
        for (offset, op) in ops {
            let class = self.class_of_stack(stack);
            push(class, at, offset);
            at = offset;
            apply(stack, &op)?;
        }
        let class = self.class_of_stack(stack);
        push(class, at, text.len());
        Some(spans)
    }
}

fn apply(stack: &mut ScopeStack, op: &ScopeStackOp) -> Option<()> {
    stack.apply(op).ok()
}

/// Highlight one file. `None` when no grammar knows its path, or it's too
/// large to be worth it.
pub fn highlight_file(file: &FileDiff) -> Option<FileHighlights> {
    let lines: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
    if file.binary || lines == 0 || lines > MAX_LINES {
        return None;
    }
    let set = syntaxes();
    let syntax = syntax_for(set, &file.path)?;
    let mut classifier = Classifier { cache: HashMap::new() };
    let mut out = FileHighlights { hunks: Vec::with_capacity(file.hunks.len()) };
    for hunk in &file.hunks {
        let mut spans = vec![Vec::new(); hunk.lines.len()];
        for side in [LineKind::Deleted, LineKind::Added] {
            let mut state = ParseState::new(syntax);
            let mut stack = ScopeStack::new();
            for (at, line) in hunk.lines.iter().enumerate() {
                if line.kind != side && line.kind != LineKind::Context {
                    continue;
                }
                if line.text.len() > MAX_LINE_LEN {
                    continue;
                }
                let Some(line_spans) = classifier.line(set, &mut state, &mut stack, &line.text) else {
                    break;
                };
                // A context line takes the new side's reading.
                if line.kind != LineKind::Context || side == LineKind::Added {
                    spans[at] = line_spans;
                }
            }
        }
        out.hunks.push(spans);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse::parse_patch;

    fn file(path: &str, body: &str) -> FileDiff {
        let patch = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,99 +1,99 @@\n{body}");
        parse_patch(&patch).remove(0)
    }

    fn class_at(text: &str, spans: &[SyntaxSpan], word: &str) -> Option<SyntaxClass> {
        let at = text.find(word)? as u32;
        spans.iter().find(|s| s.range.contains(&at)).map(|s| s.class)
    }

    #[test]
    fn rust_tokens_get_classes() {
        let f = file("src/x.rs", "+fn main() { let s = \"hi\"; // note\n");
        let h = highlight_file(&f).unwrap();
        let (text, spans) = (&f.hunks[0].lines[0].text, h.line(0, 0));
        assert_eq!(class_at(text, spans, "fn"), Some(SyntaxClass::Keyword));
        assert_eq!(class_at(text, spans, "main"), Some(SyntaxClass::Function));
        assert_eq!(class_at(text, spans, "hi"), Some(SyntaxClass::String));
        assert_eq!(class_at(text, spans, "note"), Some(SyntaxClass::Comment));
    }

    #[test]
    fn the_bundle_knows_typescript_and_toml() {
        let ts = file("a.ts", "+const x: number = 1;\n");
        let h = highlight_file(&ts).unwrap();
        assert_eq!(class_at(&ts.hunks[0].lines[0].text, h.line(0, 0), "const"), Some(SyntaxClass::Keyword));
        let toml = file("Cargo.toml", "+name = \"x\"\n");
        let h = highlight_file(&toml).unwrap();
        assert_eq!(class_at(&toml.hunks[0].lines[0].text, h.line(0, 0), "\"x\""), Some(SyntaxClass::String));
    }

    #[test]
    fn each_side_of_a_hunk_is_read_on_its_own() {
        // The deleted line opens a comment; the added lines after it aren't in it.
        let f = file("x.rs", "-/* open\n+let a = 1;\n let b = 2;\n");
        let h = highlight_file(&f).unwrap();
        let lines = &f.hunks[0].lines;
        assert_eq!(class_at(&lines[0].text, h.line(0, 0), "open"), Some(SyntaxClass::Comment));
        assert_eq!(class_at(&lines[1].text, h.line(0, 1), "let"), Some(SyntaxClass::Keyword));
        assert_eq!(class_at(&lines[2].text, h.line(0, 2), "let"), Some(SyntaxClass::Keyword));
    }

    #[test]
    fn spans_stay_on_the_lines_text() {
        let f = file("x.py", "+def f(a):\n+    return a  # é done\n");
        let h = highlight_file(&f).unwrap();
        for (at, line) in f.hunks[0].lines.iter().enumerate() {
            for s in h.line(0, at) {
                assert!(line.text.get(s.range.start as usize..s.range.end as usize).is_some(), "{s:?}");
            }
        }
    }

    #[test]
    fn unknown_paths_and_binary_files_stay_plain() {
        assert!(highlight_file(&file("data.unknownext", "+x\n")).is_none());
        let bin = parse_patch("diff --git a/i.png b/i.png\nBinary files a/i.png and b/i.png differ\n").remove(0);
        assert!(highlight_file(&bin).is_none());
    }
}
