//! Inline Markdown inside one row: which bytes are syntax (hidden while
//! editing), which are styled, and the marks an edit has to keep balanced.
//!
//! A pragmatic GitHub-flavored subset: backslash escapes, code spans,
//! `**strong**` / `*emphasis*` (and `_` forms, CommonMark's flanking rules),
//! `~~strikethrough~~`, `[links](url)`, `<autolinks>` and bare URLs. Images
//! and inline HTML are left visible as their source (muted), so nothing the
//! subset doesn't know is hidden or lost. Whatever this gets wrong only
//! changes how text looks: the source is never rewritten by parsing.

use std::ops::Range;

/// How a stretch of visible text is drawn. Spans nest, so a byte can be
/// bold and a link at once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
    /// Source this editor doesn't render (an image, inline HTML): shown as
    /// written, muted.
    pub raw: bool,
}

impl Style {
    fn union(self, o: Style) -> Style {
        Style {
            bold: self.bold || o.bold,
            italic: self.italic || o.italic,
            strike: self.strike || o.strike,
            code: self.code || o.code,
            link: self.link || o.link,
            raw: self.raw || o.raw,
        }
    }
}

/// A formatting command's mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkKind {
    Bold,
    Italic,
    Strike,
    Code,
    Link,
    /// A backslash escape: `open` is the backslash, the content the
    /// escaped character.
    Escape,
}

impl MarkKind {
    fn style(self) -> Style {
        let mut s = Style::default();
        match self {
            MarkKind::Bold => s.bold = true,
            MarkKind::Italic => s.italic = true,
            MarkKind::Strike => s.strike = true,
            MarkKind::Code => s.code = true,
            MarkKind::Link => s.link = true,
            MarkKind::Escape => {}
        }
        s
    }
}

/// One piece of inline syntax around content: its opening and closing
/// delimiters (hidden) and the content between them, in the coordinates of
/// the text it was parsed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub kind: MarkKind,
    pub open: Range<usize>,
    pub close: Range<usize>,
    /// A link's destination as written (empty for the other kinds).
    pub url: String,
}

impl Mark {
    /// The content between the delimiters.
    pub fn content(&self) -> Range<usize> {
        self.open.end..self.close.start
    }

    /// The whole mark, delimiters included.
    pub fn whole(&self) -> Range<usize> {
        self.open.start..self.close.end
    }
}

/// What one row's text parses into.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inline {
    /// Syntax bytes, hidden while editing: sorted, disjoint.
    pub hidden: Vec<Range<usize>>,
    /// Styled stretches (possibly nested).
    pub spans: Vec<(Range<usize>, Style)>,
    /// Each mark, in the order they close.
    pub marks: Vec<Mark>,
    /// The `\n`s that are hard line breaks (shown as a new line rather than
    /// a space).
    pub breaks: Vec<usize>,
}

impl Inline {
    /// The style of each visible stretch, flattened: disjoint ranges in
    /// order, each with every span covering it combined.
    pub fn flat_spans(&self) -> Vec<(Range<usize>, Style)> {
        let mut cuts: Vec<usize> = self
            .spans
            .iter()
            .flat_map(|(r, _)| [r.start, r.end])
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        let mut out: Vec<(Range<usize>, Style)> = Vec::new();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let style = self
                .spans
                .iter()
                .filter(|(r, _)| r.start <= a && b <= r.end)
                .fold(Style::default(), |acc, (_, s)| acc.union(*s));
            if style == Style::default() {
                continue;
            }
            match out.last_mut() {
                Some((r, s)) if r.end == a && *s == style => r.end = b,
                _ => out.push((a..b, style)),
            }
        }
        out
    }
}

/// Parse `text` (one row's content, `\n` between its lines).
pub fn parse(text: &str) -> Inline {
    let mut out = Inline::default();
    parse_into(text, 0..text.len(), &mut out);
    out.hidden.sort_by_key(|r| r.start);
    // Merge touching ranges (a closer right before an opener).
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(out.hidden.len());
    for r in out.hidden.drain(..) {
        if r.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some(last) if last.end >= r.start => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    out.hidden = merged;
    out.breaks.sort_unstable();
    out
}

/// A run of `*`, `_` or `~` waiting to be matched.
#[derive(Debug, Clone)]
struct Delim {
    ch: u8,
    /// Where the unconsumed part of the run starts, and its length.
    pos: usize,
    len: usize,
    /// The run's length as written (the rule of three).
    orig: usize,
    can_open: bool,
    can_close: bool,
    active: bool,
}

fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace())
}

fn parse_into(text: &str, range: Range<usize>, out: &mut Inline) {
    let bytes = text.as_bytes();
    let mut delims: Vec<Delim> = Vec::new();
    let mut i = range.start;
    let end = range.end;
    while i < end {
        let b = bytes[i];
        match b {
            b'\\' => {
                let next = text[i + 1..end].chars().next();
                match next {
                    Some('\n') => {
                        out.hidden.push(i..i + 1);
                        out.breaks.push(i + 1);
                        i += 2;
                    }
                    Some(c) if c.is_ascii_punctuation() => {
                        out.hidden.push(i..i + 1);
                        out.marks.push(Mark {
                            kind: MarkKind::Escape,
                            open: i..i + 1,
                            close: i + 2..i + 2,
                            url: String::new(),
                        });
                        i += 2;
                    }
                    _ => i += 1,
                }
            }
            b'`' => {
                let n = run_len(bytes, i, end, b'`');
                match find_backtick_run(bytes, i + n, end, n) {
                    Some(j) => {
                        out.hidden.push(i..i + n);
                        out.hidden.push(j..j + n);
                        out.spans.push((i + n..j, MarkKind::Code.style()));
                        out.marks.push(Mark {
                            kind: MarkKind::Code,
                            open: i..i + n,
                            close: j..j + n,
                            url: String::new(),
                        });
                        i = j + n;
                    }
                    None => i += n,
                }
            }
            b'<' => {
                if let Some(close) = autolink(text, i, end) {
                    out.hidden.push(i..i + 1);
                    out.hidden.push(close..close + 1);
                    out.spans.push((i + 1..close, MarkKind::Link.style()));
                    out.marks.push(Mark {
                        kind: MarkKind::Link,
                        open: i..i + 1,
                        close: close..close + 1,
                        url: text[i + 1..close].to_string(),
                    });
                    i = close + 1;
                } else if let Some(close) = html_tag(text, i, end) {
                    out.spans.push((
                        i..close,
                        Style {
                            raw: true,
                            ..Style::default()
                        },
                    ));
                    i = close;
                } else {
                    i += 1;
                }
            }
            b'!' if bytes.get(i + 1) == Some(&b'[') && i + 1 < end => {
                match link_at(text, i + 1, end) {
                    Some(l) => {
                        out.spans.push((
                            i..l.end,
                            Style {
                                raw: true,
                                ..Style::default()
                            },
                        ));
                        i = l.end;
                    }
                    None => i += 1,
                }
            }
            b'[' => match link_at(text, i, end) {
                Some(l) => {
                    out.hidden.push(i..i + 1);
                    out.hidden.push(l.text_end..l.end);
                    out.spans.push((i + 1..l.text_end, MarkKind::Link.style()));
                    out.marks.push(Mark {
                        kind: MarkKind::Link,
                        open: i..i + 1,
                        close: l.text_end..l.end,
                        url: l.url,
                    });
                    parse_into(text, i + 1..l.text_end, out);
                    i = l.end;
                }
                None => i += 1,
            },
            b'*' | b'_' | b'~' => {
                let n = run_len(bytes, i, end, b);
                let before = text[range.start..i].chars().next_back();
                let after = text[i + n..end].chars().next();
                let ws_before = before.is_none_or(char::is_whitespace);
                let ws_after = after.is_none_or(char::is_whitespace);
                let p_before = before.is_some_and(is_punct);
                let p_after = after.is_some_and(is_punct);
                let left = !ws_after && (!p_after || ws_before || p_before);
                let right = !ws_before && (!p_before || ws_after || p_after);
                let (can_open, can_close) = match b {
                    b'_' => (left && (!right || p_before), right && (!left || p_after)),
                    _ => (left, right),
                };
                // GitHub's strikethrough takes one or two tildes, never more.
                if b != b'~' || n <= 2 {
                    delims.push(Delim {
                        ch: b,
                        pos: i,
                        len: n,
                        orig: n,
                        can_open,
                        can_close,
                        active: true,
                    });
                }
                i += n;
            }
            b'h' | b'w' => {
                let at_word_start = text[range.start..i]
                    .chars()
                    .next_back()
                    .is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '*' | '_' | '~'));
                match at_word_start.then(|| bare_url(text, i, end)).flatten() {
                    Some(stop) => {
                        out.spans.push((i..stop, MarkKind::Link.style()));
                        i = stop;
                    }
                    None => i += 1,
                }
            }
            b'\n' => {
                // Two trailing spaces make a hard break too.
                if text[range.start..i].ends_with("  ") {
                    out.breaks.push(i);
                }
                i += 1;
            }
            _ => i += text[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    process_emphasis(&mut delims, out);
}

fn run_len(bytes: &[u8], i: usize, end: usize, b: u8) -> usize {
    bytes[i..end].iter().take_while(|&&c| c == b).count()
}

fn find_backtick_run(bytes: &[u8], from: usize, end: usize, n: usize) -> Option<usize> {
    let mut j = from;
    while j < end {
        if bytes[j] == b'`' {
            let m = run_len(bytes, j, end, b'`');
            if m == n {
                return Some(j);
            }
            j += m;
        } else {
            j += 1;
        }
    }
    None
}

/// `<scheme:...>` or `<user@host>`: the index of its `>`.
fn autolink(text: &str, i: usize, end: usize) -> Option<usize> {
    let rest = &text[i + 1..end];
    let close = rest.find('>')?;
    let inner = &rest[..close];
    if inner.is_empty() || inner.contains(|c: char| c.is_whitespace() || c == '<') {
        return None;
    }
    let scheme = inner.split(':').next().unwrap_or("");
    let uri = inner.contains(':')
        && (2..=32).contains(&scheme.len())
        && scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    let email = !inner.contains(':')
        && inner.contains('@')
        && !inner.starts_with('@')
        && !inner.ends_with('@');
    (uri || email).then_some(i + 1 + close)
}

/// An inline HTML tag (`<b>`, `</b>`, `<!-- x -->`, `<br/>`): where it ends.
fn html_tag(text: &str, i: usize, end: usize) -> Option<usize> {
    let rest = &text[i + 1..end];
    let first = rest.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '/' || first == '!') {
        return None;
    }
    let close = rest.find('>')?;
    if rest[..close].contains('<') {
        return None;
    }
    Some(i + 1 + close + 1)
}

struct LinkParts {
    /// The index of the text's `]`.
    text_end: usize,
    /// One past the `)`.
    end: usize,
    url: String,
}

/// `[text](destination "title")` starting at the `[` at `i`.
fn link_at(text: &str, i: usize, end: usize) -> Option<LinkParts> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut j = i + 1;
    let text_end = loop {
        if j >= end {
            return None;
        }
        match bytes[j] {
            b'\\' => j += 2,
            b'`' => {
                let n = run_len(bytes, j, end, b'`');
                j = find_backtick_run(bytes, j + n, end, n).map_or(j + n, |k| k + n);
            }
            b'[' => {
                depth += 1;
                j += 1;
            }
            b']' if depth == 0 => break j,
            b']' => {
                depth -= 1;
                j += 1;
            }
            _ => j += 1,
        }
    };
    if bytes.get(text_end + 1) != Some(&b'(') || text_end + 1 >= end {
        return None;
    }
    let mut k = text_end + 2;
    let mut parens = 0usize;
    let mut in_angle = false;
    let dest_start = k;
    let mut dest_end = None;
    while k < end {
        match bytes[k] {
            b'\\' => k += 2,
            b'<' if k == dest_start => {
                in_angle = true;
                k += 1;
            }
            b'>' if in_angle => {
                in_angle = false;
                k += 1;
            }
            b'(' if !in_angle => {
                parens += 1;
                k += 1;
            }
            b')' if !in_angle && parens == 0 => {
                let dest = dest_end.unwrap_or(k);
                let url = text[dest_start..dest]
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string();
                return Some(LinkParts {
                    text_end,
                    end: k + 1,
                    url,
                });
            }
            b')' if !in_angle => {
                parens -= 1;
                k += 1;
            }
            b' ' | b'\n' | b'\t' if !in_angle && dest_end.is_none() => {
                dest_end = Some(k);
                k += 1;
            }
            _ => k += 1,
        }
    }
    None
}

/// A bare `http(s)://` or `www.` URL at `i` (GitHub's autolink literal):
/// where it stops, trailing punctuation left out.
fn bare_url(text: &str, i: usize, end: usize) -> Option<usize> {
    let rest = &text[i..end];
    let prefix = ["https://", "http://", "www."]
        .into_iter()
        .find(|p| rest.starts_with(p))?;
    let mut stop = rest
        .find(|c: char| c.is_whitespace() || c == '<')
        .unwrap_or(rest.len());
    loop {
        let tail = rest[..stop].chars().next_back()?;
        let unbalanced_paren =
            tail == ')' && rest[..stop].matches('(').count() < rest[..stop].matches(')').count();
        if matches!(
            tail,
            '?' | '!' | '.' | ',' | ':' | '*' | '_' | '~' | '\'' | '"'
        ) || unbalanced_paren
        {
            stop -= tail.len_utf8();
        } else {
            break;
        }
    }
    (stop > prefix.len()).then_some(i + stop)
}

/// Match the delimiter runs (CommonMark's "process emphasis", without the
/// bottom-of-stack optimization).
fn process_emphasis(delims: &mut [Delim], out: &mut Inline) {
    let mut c = 0;
    while c < delims.len() {
        if !delims[c].active || !delims[c].can_close || delims[c].len == 0 {
            c += 1;
            continue;
        }
        let closer = delims[c].clone();
        let opener = (0..c).rev().find(|&o| {
            let d = &delims[o];
            if !d.active || !d.can_open || d.ch != closer.ch || d.len == 0 {
                return false;
            }
            if closer.ch == b'~' {
                return d.len == closer.len;
            }
            let odd = (d.can_close || closer.can_open)
                && (d.orig + closer.orig).is_multiple_of(3)
                && !(d.orig.is_multiple_of(3) && closer.orig.is_multiple_of(3));
            !odd
        });
        let Some(o) = opener else {
            c += 1;
            continue;
        };
        let n = if closer.ch == b'~' {
            closer.len
        } else if delims[o].len >= 2 && closer.len >= 2 {
            2
        } else {
            1
        };
        let kind = match (closer.ch, n) {
            (b'~', _) => MarkKind::Strike,
            (_, 2) => MarkKind::Bold,
            _ => MarkKind::Italic,
        };
        let open_end = delims[o].pos + delims[o].len;
        let open = open_end - n..open_end;
        let close = closer.pos..closer.pos + n;
        out.hidden.push(open.clone());
        out.hidden.push(close.clone());
        out.spans.push((open.end..close.start, kind.style()));
        out.marks.push(Mark {
            kind,
            open,
            close,
            url: String::new(),
        });
        delims[o].len -= n;
        for d in &mut delims[o + 1..c] {
            d.active = false;
        }
        delims[c].pos += n;
        delims[c].len -= n;
        if delims[c].len == 0 {
            c += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The visible text: `text` without its hidden bytes.
    fn shown(text: &str) -> String {
        let p = parse(text);
        let mut out = String::new();
        let mut at = 0;
        for r in &p.hidden {
            out.push_str(&text[at..r.start]);
            at = r.end;
        }
        out.push_str(&text[at..]);
        out
    }

    fn kinds(text: &str) -> Vec<(MarkKind, String)> {
        let p = parse(text);
        let mut marks = p.marks.clone();
        marks.sort_by_key(|m| m.open.start);
        marks
            .into_iter()
            .map(|m| (m.kind, text[m.content()].to_string()))
            .collect()
    }

    #[test]
    fn emphasis_strong_strike_and_code_hide_their_delimiters() {
        assert_eq!(
            shown("a **bold** and *it* and `code`"),
            "a bold and it and code"
        );
        assert_eq!(shown("~~gone~~ __strong__ _em_"), "gone strong em");
        assert_eq!(
            kinds("**b** *i* ~~s~~ `c`"),
            [
                (MarkKind::Bold, "b".into()),
                (MarkKind::Italic, "i".into()),
                (MarkKind::Strike, "s".into()),
                (MarkKind::Code, "c".into())
            ]
        );
        assert_eq!(shown("***both***"), "both");
    }

    #[test]
    fn intraword_underscores_and_lone_stars_stay_text() {
        assert_eq!(shown("snake_case_name"), "snake_case_name");
        assert_eq!(shown("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(shown("**unclosed"), "**unclosed");
        assert_eq!(shown("a ** b"), "a ** b");
    }

    #[test]
    fn escapes_hide_the_backslash_only() {
        assert_eq!(shown(r"\*not em\* and \\"), r"*not em* and \");
        assert_eq!(shown(r"C:\path"), r"C:\path");
        assert_eq!(kinds(r"\#")[0].0, MarkKind::Escape);
    }

    #[test]
    fn code_spans_keep_their_contents_literal() {
        assert_eq!(shown("`**x**` and ``a ` b``"), "**x** and a ` b");
        assert_eq!(shown("`unclosed"), "`unclosed");
    }

    #[test]
    fn links_show_their_text_and_keep_the_destination() {
        assert_eq!(
            shown("see [the **plan**](https://x.dev/a_(b)) now"),
            "see the plan now"
        );
        let p = parse("[docs](https://x.dev \"Title\")");
        let link = p.marks.iter().find(|m| m.kind == MarkKind::Link).unwrap();
        assert_eq!(link.url, "https://x.dev");
        assert_eq!(shown("<https://x.dev>"), "https://x.dev");
        assert_eq!(shown("[not a link] (x)"), "[not a link] (x)");
    }

    #[test]
    fn bare_urls_are_links_without_hiding_anything() {
        let text = "go to https://example.com/path. Then";
        let p = parse(text);
        assert!(p.hidden.is_empty());
        let (r, s) = &p.spans[0];
        assert_eq!(&text[r.clone()], "https://example.com/path");
        assert!(s.link);
    }

    #[test]
    fn images_and_html_stay_visible_as_source() {
        assert_eq!(shown("![alt](a.png) <b>x</b>"), "![alt](a.png) <b>x</b>");
        let p = parse("![alt](a.png)");
        assert!(p.spans[0].1.raw);
    }

    #[test]
    fn hard_breaks_are_found() {
        let p = parse("one\\\ntwo  \nthree\nfour");
        assert_eq!(p.breaks, vec![4, 10]);
        assert_eq!(shown("one\\\ntwo"), "one\ntwo");
    }

    #[test]
    fn nested_spans_flatten_to_disjoint_styles() {
        let text = "**a *b* c**";
        let flat = parse(text).flat_spans();
        let styles: Vec<(&str, bool, bool)> = flat
            .iter()
            .map(|(r, s)| (&text[r.clone()], s.bold, s.italic))
            .collect();
        // Ranges are in the parsed text, so the hidden `*`s ride along.
        assert_eq!(
            styles,
            [
                ("a *", true, false),
                ("b", true, true),
                ("* c", true, false)
            ]
        );
    }
}
