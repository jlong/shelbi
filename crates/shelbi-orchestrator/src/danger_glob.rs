//! Glob matching for Zen danger paths.
//!
//! A small stand-in for the `globset` crate with its default options, which
//! is what [`crate::zen::match_danger_paths`] used before. The parser is a
//! port of globset 0.4's, so a pattern means here what it meant there:
//!
//! - `?` matches one byte, `*` matches any run of bytes, and neither stops at
//!   `/` (globset's `literal_separator(false)`). `*.yaml` matches
//!   `a/b/c.yaml`; the built-in danger paths rely on that.
//! - `**` is recursive only as a whole path component: `**/x` (`x` at any
//!   depth), `x/**` (anything under `x`, not `x` itself), `a/**/b` (zero or
//!   more directories between). A lone `**` matches everything. Anywhere
//!   else it is two `*`s.
//! - `[abc]`, `[a-z]`, `[!a]` / `[^a]` match one byte. A `]` or `-` first in
//!   the class, or a `-` last, is literal.
//! - `{a,b}` matches either branch; branches nest. An empty branch is
//!   dropped, so `foo{,.txt}` matches only `foo.txt`.
//! - `\` escapes the next character.
//! - An unclosed `[` or `{`, a stray `}`, a trailing `\`, or a backwards
//!   range is an error.
//!
//! globset matched on bytes, so a class holding a non-ASCII character
//! tests the bytes of its UTF-8 encoding one at a time, not the character.
//! That is kept as is rather than "fixed", so no verdict changes with the
//! swap. Two deliberate differences, both out of reach of a real `git diff
//! --name-only` path: `*` and `?` also match `\n` (git quotes such names,
//! and globset itself disagreed with its own regex there), and a class
//! whose byte ranges come out backwards is an error for that pattern alone,
//! where globset failed the whole set.

/// One compiled danger-path glob.
#[derive(Debug, Clone)]
pub(crate) struct Glob {
    tokens: Vec<Token>,
}

/// Why a pattern isn't a glob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlobError {
    UnclosedClass,
    InvalidRange,
    UnopenedAlternates,
    UnclosedAlternates,
    DanglingEscape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(char),
    /// `?`
    Any,
    /// `*`
    ZeroOrMore,
    /// A leading `**/`: nothing, or anything ending in `/`.
    RecursivePrefix,
    /// A trailing `/**`: `/` and then anything.
    RecursiveSuffix,
    /// An inner `/**/`: `/`, or `/` ... `/`.
    RecursiveZeroOrMore,
    Class(ByteSet),
    Alternates(Vec<Vec<Token>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByteSet([u64; 4]);

impl ByteSet {
    fn insert_range(&mut self, lo: u8, hi: u8) {
        for b in lo..=hi {
            self.0[usize::from(b / 64)] |= 1 << (b % 64);
        }
    }

    fn contains(&self, b: u8) -> bool {
        self.0[usize::from(b / 64)] & (1 << (b % 64)) != 0
    }

    fn negate(&mut self) {
        for word in &mut self.0 {
            *word = !*word;
        }
    }
}

impl Glob {
    pub(crate) fn new(pattern: &str) -> Result<Self, GlobError> {
        let mut p = Parser {
            chars: pattern.chars().peekable(),
            prev: None,
            cur: None,
            alternates_stack: Vec::new(),
            branches: vec![Vec::new()],
        };
        p.parse()?;
        if p.branches.len() > 1 {
            return Err(GlobError::UnclosedAlternates);
        }
        let mut tokens = p.branches.pop().unwrap_or_default();
        drop_empty_branches(&mut tokens);
        Ok(Self { tokens })
    }

    pub(crate) fn is_match(&self, path: &str) -> bool {
        // globset special-cases a glob that is only `**`.
        if self.tokens == [Token::RecursivePrefix] {
            return true;
        }
        let path = path.as_bytes();
        let mut start = vec![false; path.len() + 1];
        start[0] = true;
        advance(&self.tokens, path, start)[path.len()]
    }
}

/// globset leaves out a branch that compiles to nothing (an empty one, or
/// one made only of such groups); a group with no branches left matches
/// the empty string.
fn drop_empty_branches(tokens: &mut [Token]) {
    for tok in tokens {
        if let Token::Alternates(branches) = tok {
            for b in branches.iter_mut() {
                drop_empty_branches(b);
            }
            branches.retain(|b| {
                !b.iter()
                    .all(|t| matches!(t, Token::Alternates(inner) if inner.is_empty()))
            });
        }
    }
}

/// Run `tokens` over `path`: `at[i]` says the match so far can end at byte
/// `i`; the result says where the whole sequence can end. Position sets
/// instead of backtracking keep `*a*a*a` linear in the tokens.
fn advance(tokens: &[Token], path: &[u8], mut at: Vec<bool>) -> Vec<bool> {
    let n = path.len();
    for tok in tokens {
        let mut next = vec![false; n + 1];
        // Ends after a `/` that sits at or past byte `from`.
        let after_slash = |next: &mut [bool], from: usize| {
            for q in from + 1..=n {
                if path[q - 1] == b'/' {
                    next[q] = true;
                }
            }
        };
        match tok {
            Token::Alternates(branches) if branches.is_empty() => next = at.clone(),
            Token::Alternates(branches) => {
                for b in branches {
                    for (q, hit) in advance(b, path, at.clone()).into_iter().enumerate() {
                        next[q] |= hit;
                    }
                }
            }
            Token::ZeroOrMore => {
                if let Some(p) = at.iter().position(|&hit| hit) {
                    next[p..].fill(true);
                }
            }
            _ => {
                for p in (0..=n).filter(|&p| at[p]) {
                    match tok {
                        Token::Literal(c) => {
                            let mut buf = [0; 4];
                            let lit = c.encode_utf8(&mut buf).as_bytes();
                            if path[p..].starts_with(lit) {
                                next[p + lit.len()] = true;
                            }
                        }
                        Token::Any => {
                            if p < n {
                                next[p + 1] = true;
                            }
                        }
                        Token::Class(set) => {
                            if p < n && set.contains(path[p]) {
                                next[p + 1] = true;
                            }
                        }
                        Token::RecursivePrefix => {
                            next[p] = true;
                            after_slash(&mut next, p);
                        }
                        Token::RecursiveSuffix => {
                            if path.get(p) == Some(&b'/') {
                                next[p + 1..].fill(true);
                            }
                        }
                        Token::RecursiveZeroOrMore => {
                            if path.get(p) == Some(&b'/') {
                                after_slash(&mut next, p);
                            }
                        }
                        Token::ZeroOrMore | Token::Alternates(_) => unreachable!(),
                    }
                }
            }
        }
        if !next.contains(&true) {
            return next;
        }
        at = next;
    }
    at
}

/// globset's parser, minus its options (only the defaults on Unix exist
/// here: `/` is the only separator and `\` escapes).
struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    prev: Option<char>,
    cur: Option<char>,
    /// Index into `branches` where each open `{` started.
    alternates_stack: Vec<usize>,
    /// The branches being parsed; tokens go on the last. Never empty.
    branches: Vec<Vec<Token>>,
}

impl Parser<'_> {
    fn parse(&mut self) -> Result<(), GlobError> {
        while let Some(c) = self.bump() {
            match c {
                '?' => self.push(Token::Any),
                '*' => self.parse_star(),
                '[' => self.parse_class()?,
                '{' => {
                    self.alternates_stack.push(self.branches.len());
                    self.branches.push(Vec::new());
                }
                '}' => {
                    let start = self
                        .alternates_stack
                        .pop()
                        .ok_or(GlobError::UnopenedAlternates)?;
                    let alts = self.branches.drain(start..).collect();
                    self.push(Token::Alternates(alts));
                }
                ',' if !self.alternates_stack.is_empty() => self.branches.push(Vec::new()),
                '\\' => {
                    let c = self.bump().ok_or(GlobError::DanglingEscape)?;
                    self.push(Token::Literal(c));
                }
                c => self.push(Token::Literal(c)),
            }
        }
        Ok(())
    }

    fn branch(&mut self) -> &mut Vec<Token> {
        self.branches
            .last_mut()
            .expect("the root branch is never drained")
    }

    fn push(&mut self, tok: Token) {
        self.branch().push(tok);
    }

    fn bump(&mut self) -> Option<char> {
        self.prev = self.cur;
        self.cur = self.chars.next();
        self.cur
    }

    fn parse_star(&mut self) {
        let prev = self.prev;
        if self.chars.peek() != Some(&'*') {
            self.push(Token::ZeroOrMore);
            return;
        }
        self.bump();
        // Two `ZeroOrMore`s where globset has two, so a later `/**` that
        // pops one leaves the other, as it does there.
        let two_stars = |p: &mut Self| {
            p.push(Token::ZeroOrMore);
            p.push(Token::ZeroOrMore);
        };
        if self.branch().is_empty() {
            if self.chars.peek().is_some_and(|&c| c != '/') {
                two_stars(self);
            } else {
                self.push(Token::RecursivePrefix);
                self.bump();
            }
            return;
        }
        if prev != Some('/')
            && (self.branches.len() <= 1 || (prev != Some(',') && prev != Some('{')))
        {
            two_stars(self);
            return;
        }
        let is_suffix = match self.chars.peek() {
            None => true,
            Some(',' | '}') if self.branches.len() >= 2 => true,
            Some('/') => {
                self.bump();
                false
            }
            _ => {
                two_stars(self);
                return;
            }
        };
        let last = self.branch().pop().expect("checked non-empty above");
        self.push(match last {
            Token::RecursivePrefix => Token::RecursivePrefix,
            Token::RecursiveSuffix => Token::RecursiveSuffix,
            _ if is_suffix => Token::RecursiveSuffix,
            _ => Token::RecursiveZeroOrMore,
        });
    }

    fn parse_class(&mut self) -> Result<(), GlobError> {
        let mut ranges: Vec<(char, char)> = Vec::new();
        let negated = matches!(self.chars.peek(), Some('!' | '^'));
        if negated {
            self.bump();
        }
        // `a-b` widens the last range to `b`; a backwards one is an error.
        let widen = |ranges: &mut Vec<(char, char)>, c: char| {
            let r = ranges.last_mut().expect("a range is open");
            r.1 = c;
            if r.1 < r.0 {
                Err(GlobError::InvalidRange)
            } else {
                Ok(())
            }
        };
        let mut first = true;
        let mut in_range = false;
        loop {
            let c = self.bump().ok_or(GlobError::UnclosedClass)?;
            match c {
                ']' if !first => break,
                '-' if first => ranges.push(('-', '-')),
                '-' if in_range => {
                    widen(&mut ranges, '-')?;
                    in_range = false;
                }
                '-' => in_range = true,
                c if in_range => {
                    widen(&mut ranges, c)?;
                    in_range = false;
                }
                c => ranges.push((c, c)),
            }
            first = false;
        }
        if in_range {
            ranges.push(('-', '-'));
        }
        let mut set = class_bytes(&ranges)?;
        if negated {
            set.negate();
        }
        self.push(Token::Class(set));
        Ok(())
    }
}

/// The bytes a class matches, read the way globset's regex did: each
/// character is written out as its UTF-8 bytes, so in a range of two
/// multi-byte characters only the last byte of one and the first of the
/// other form the range.
fn class_bytes(ranges: &[(char, char)]) -> Result<ByteSet, GlobError> {
    let mut set = ByteSet([0; 4]);
    for &(lo, hi) in ranges {
        let (mut a, mut z) = ([0; 4], [0; 4]);
        let lo_bytes = lo.encode_utf8(&mut a).as_bytes();
        if lo == hi {
            lo_bytes.iter().for_each(|&b| set.insert_range(b, b));
            continue;
        }
        let hi_bytes = hi.encode_utf8(&mut z).as_bytes();
        let (lo_last, lo_rest) = lo_bytes.split_last().expect("a char has bytes");
        let (hi_first, hi_rest) = hi_bytes.split_first().expect("a char has bytes");
        if lo_last > hi_first {
            return Err(GlobError::InvalidRange);
        }
        lo_rest.iter().for_each(|&b| set.insert_range(b, b));
        set.insert_range(*lo_last, *hi_first);
        hi_rest.iter().for_each(|&b| set.insert_range(b, b));
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::ProjectShape;

    /// Every shape, so each preset's patterns are in the table.
    const SHAPES: [ProjectShape; 5] = [
        ProjectShape::CargoWorkspace,
        ProjectShape::Node,
        ProjectShape::GitHub,
        ProjectShape::Docker,
        ProjectShape::Shelbi,
    ];

    /// Paths every pattern is run against.
    const CORPUS: &[&str] = &[
        "", "a", "b", "c", "x", "ab", "abc", "abd", "a/b", "a/c", "a/x/b", "a/x/y/b", "a/b/c",
        "b/c", "x/a", "x/b/a", "foo", "foo.txt", "foo.txtx", "a.yaml", "a/b/c.yaml", ".yaml",
        "a.yaml/b", "x.yml", ".github/workflows/ci.yml", ".github/workflows",
        ".github/workflows/", ".github/CODEOWNERS", ".github/dependabot.yml",
        ".github/x/y.yml", "Cargo.lock", "crates/x/Cargo.lock", "Cargo.toml",
        "crates/x/Cargo.toml", "rust-toolchain.toml", ".cargo/config.toml", "next.config.js",
        "next.config.mjs", "web/next.config.js", "vercel.json", ".npmrc", "package.json",
        "package-lock.json", "web/package-lock.json", ".shelbi", ".shelbi/x", ".shelbi/a/b",
        "x/.shelbi/y", "shelbi.yaml", ".env", ".env.local", "config/.env", "src/main.rs",
        "src/a/test1.rs", "src/test1.rs", "src/test12.rs", "*", "a*b", "]a", "-", "a-", "^",
        "!a", "x/", "/x", "/", "//", "café", "cafe", "cafx", "éx", "é", "a,b", "{a,b}",
        "LICENSE", "LICENSE.md", "scripts/install.sh", "scripts/install.shx", "Dockerfile",
        "compose.yaml", "docs/compose.yaml",
    ];

    /// Edge cases beyond the shipped presets.
    const EDGE_PATTERNS: &[&str] = &[
        "**", "**/", "**/**", "**/x", "**/a/b", "x/**", "a/**/b", "a/**/**", "**/*.rs",
        "src/**/test?.rs", "*", "?", "??", ".*", "a?c", "a*b", "a**b", "**a", "a/**b",
        "[abc]", "[!a]", "[^a]*", "[a-c]b", "[]a]", "[!]a]*", "[-a]", "[a-]", "[a-b-c]",
        "[^]", "{a,b}", "{a,b}/c", "foo{,.txt}", "{,}", "{{,},a}", "{*.yaml,*.yml}",
        "{a,{b,c}}", "{**/a,b}", "a{b,**}", "x{/**,}", "\\*", "a\\/**", "\\{a,b\\}",
        ".github/**/*.yml", "**/.env", ".env*", "café", "caf?", "caf[eé]", "*é", "[é]x",
        "[a-é]", "a,b", "",
        // Not globs.
        "[abc", "{a,b", "a}", "\\", "[z-a]", "[a--]", "[é-a]",
    ];

    /// The shipped patterns: the built-ins and every shape's preset.
    fn shipped_patterns() -> Vec<&'static str> {
        let mut out: Vec<&str> = shelbi_core::BUILTIN_DANGER_PATHS.to_vec();
        for shape in SHAPES {
            out.extend(shape.danger_paths());
        }
        out
    }

    /// Which [`CORPUS`] paths a pattern matches.
    enum Want {
        /// Not a glob.
        Invalid,
        All,
        Only(&'static [&'static str]),
        AllBut(&'static [&'static str]),
    }

    use Want::*;

    /// globset 0.4.18's answers, recorded from it before the swap (and
    /// checked against it while it was still a dependency). Every shipped
    /// pattern is here, then [`EDGE_PATTERNS`]' in order.
    const TABLE: &[(&str, Want)] = &[
        (".github/workflows/**", Only(&[".github/workflows/ci.yml", ".github/workflows/"])),
        ("scripts/install.sh", Only(&["scripts/install.sh"])),
        (
            "*.yaml",
            Only(&[
                "a.yaml", "a/b/c.yaml", ".yaml", "shelbi.yaml", "compose.yaml", "docs/compose.yaml",
            ]),
        ),
        (
            "*.yml",
            Only(&[
                "x.yml", ".github/workflows/ci.yml", ".github/dependabot.yml", ".github/x/y.yml",
            ]),
        ),
        ("LICENSE", Only(&["LICENSE"])),
        ("package-lock.json", Only(&["package-lock.json"])),
        ("Cargo.lock", Only(&["Cargo.lock"])),
        ("Cargo.toml", Only(&["Cargo.toml"])),
        ("rust-toolchain.toml", Only(&["rust-toolchain.toml"])),
        (".cargo/config.toml", Only(&[".cargo/config.toml"])),
        ("package.json", Only(&["package.json"])),
        ("next.config.*", Only(&["next.config.js", "next.config.mjs"])),
        ("vercel.json", Only(&["vercel.json"])),
        (".npmrc", Only(&[".npmrc"])),
        (".github/CODEOWNERS", Only(&[".github/CODEOWNERS"])),
        (".github/dependabot.yml", Only(&[".github/dependabot.yml"])),
        ("Dockerfile", Only(&["Dockerfile"])),
        ("compose.yaml", Only(&["compose.yaml"])),
        (".shelbi/**", Only(&[".shelbi/x", ".shelbi/a/b"])),
        ("shelbi.yaml", Only(&["shelbi.yaml"])),
        ("**", All),
        ("**/", All),
        ("**/**", All),
        ("**/x", Only(&["x", ".shelbi/x", "/x"])),
        ("**/a/b", Only(&["a/b", ".shelbi/a/b"])),
        ("x/**", Only(&["x/a", "x/b/a", "x/.shelbi/y", "x/"])),
        ("a/**/b", Only(&["a/b", "a/x/b", "a/x/y/b"])),
        ("a/**/**", Only(&["a/b", "a/c", "a/x/b", "a/x/y/b", "a/b/c", "a/b/c.yaml"])),
        ("**/*.rs", Only(&["src/main.rs", "src/a/test1.rs", "src/test1.rs", "src/test12.rs"])),
        ("src/**/test?.rs", Only(&["src/a/test1.rs", "src/test1.rs"])),
        ("*", All),
        ("?", Only(&["a", "b", "c", "x", "*", "-", "^", "/"])),
        ("??", Only(&["ab", "]a", "a-", "!a", "x/", "/x", "//", "é"])),
        (
            ".*",
            Only(&[
                ".yaml", ".github/workflows/ci.yml", ".github/workflows", ".github/workflows/",
                ".github/CODEOWNERS", ".github/dependabot.yml", ".github/x/y.yml",
                ".cargo/config.toml", ".npmrc", ".shelbi", ".shelbi/x", ".shelbi/a/b", ".env",
                ".env.local",
            ]),
        ),
        ("a?c", Only(&["abc", "a/c"])),
        ("a*b", Only(&["ab", "a/b", "a/x/b", "a/x/y/b", "a.yaml/b", "a*b", "a,b"])),
        ("a**b", Only(&["ab", "a/b", "a/x/b", "a/x/y/b", "a.yaml/b", "a*b", "a,b"])),
        ("**a", Only(&["a", "x/a", "x/b/a", "]a", "!a"])),
        ("a/**b", Only(&["a/b", "a/x/b", "a/x/y/b"])),
        ("[abc]", Only(&["a", "b", "c"])),
        ("[!a]", Only(&["b", "c", "x", "*", "-", "^", "/"])),
        (
            "[^a]*",
            AllBut(&[
                "", "a", "ab", "abc", "abd", "a/b", "a/c", "a/x/b", "a/x/y/b", "a/b/c", "a.yaml",
                "a/b/c.yaml", "a.yaml/b", "a*b", "a-", "a,b",
            ]),
        ),
        ("[a-c]b", Only(&["ab"])),
        ("[]a]", Only(&["a"])),
        (
            "[!]a]*",
            AllBut(&[
                "", "a", "ab", "abc", "abd", "a/b", "a/c", "a/x/b", "a/x/y/b", "a/b/c", "a.yaml",
                "a/b/c.yaml", "a.yaml/b", "a*b", "]a", "a-", "a,b",
            ]),
        ),
        ("[-a]", Only(&["a", "-"])),
        ("[a-]", Only(&["a", "-"])),
        ("[a-b-c]", Only(&["a", "b", "c"])),
        ("[^]", Invalid),
        ("{a,b}", Only(&["a", "b"])),
        ("{a,b}/c", Only(&["a/c", "b/c"])),
        ("foo{,.txt}", Only(&["foo.txt"])),
        ("{,}", Only(&[""])),
        ("{{,},a}", Only(&["a"])),
        (
            "{*.yaml,*.yml}",
            Only(&[
                "a.yaml", "a/b/c.yaml", ".yaml", "x.yml", ".github/workflows/ci.yml",
                ".github/dependabot.yml", ".github/x/y.yml", "shelbi.yaml", "compose.yaml",
                "docs/compose.yaml",
            ]),
        ),
        ("{a,{b,c}}", Only(&["a", "b", "c"])),
        ("{**/a,b}", Only(&["a", "b", "x/a", "x/b/a"])),
        (
            "a{b,**}",
            Only(&[
                "a", "ab", "abc", "abd", "a/b", "a/c", "a/x/b", "a/x/y/b", "a/b/c", "a.yaml",
                "a/b/c.yaml", "a.yaml/b", "a*b", "a-", "a,b",
            ]),
        ),
        ("x{/**,}", Only(&["x/a", "x/b/a", "x/.shelbi/y", "x/"])),
        ("\\*", Only(&["*"])),
        ("a\\/**", Only(&["a/b", "a/c", "a/x/b", "a/x/y/b", "a/b/c", "a/b/c.yaml"])),
        ("\\{a,b\\}", Only(&["{a,b}"])),
        (
            ".github/**/*.yml",
            Only(&[
                ".github/workflows/ci.yml", ".github/dependabot.yml", ".github/x/y.yml",
            ]),
        ),
        ("**/.env", Only(&[".env", "config/.env"])),
        (".env*", Only(&[".env", ".env.local"])),
        ("café", Only(&["café"])),
        ("caf?", Only(&["cafe", "cafx"])),
        ("caf[eé]", Only(&["cafe"])),
        ("*é", Only(&["café", "é"])),
        ("[é]x", Only(&[])),
        ("[a-é]", Only(&["a", "b", "c", "x"])),
        ("a,b", Only(&["a,b"])),
        ("", Only(&[""])),
        ("[abc", Invalid),
        ("{a,b", Invalid),
        ("a}", Invalid),
        ("\\", Invalid),
        ("[z-a]", Invalid),
        ("[a--]", Invalid),
        ("[é-a]", Invalid),
    ];

    fn want(w: &Want) -> Option<Vec<&'static str>> {
        Some(match w {
            Invalid => return None,
            All => CORPUS.to_vec(),
            Only(m) => m.to_vec(),
            AllBut(not) => CORPUS.iter().copied().filter(|p| !not.contains(p)).collect(),
        })
    }

    fn ours(pattern: &str) -> Option<Vec<&'static str>> {
        let g = Glob::new(pattern).ok()?;
        Some(CORPUS.iter().copied().filter(|p| g.is_match(p)).collect())
    }

    #[test]
    fn matches_the_recorded_globset_table() {
        let mut diffs = Vec::new();
        for (p, w) in TABLE {
            let (want, got) = (want(w), ours(p));
            if want != got {
                diffs.push(format!("{p:?}\n  want: {want:?}\n  got:  {got:?}"));
            }
        }
        assert!(diffs.is_empty(), "{}", diffs.join("\n"));
    }

    #[test]
    fn table_covers_every_shipped_and_edge_pattern() {
        let mut expected = shipped_patterns();
        expected.extend(EDGE_PATTERNS);
        let mut seen = std::collections::HashSet::new();
        expected.retain(|p| seen.insert(*p));
        let in_table: Vec<&str> = TABLE.iter().map(|(p, _)| *p).collect();
        assert_eq!(in_table, expected, "a new danger-path preset needs a TABLE row");
    }

    #[test]
    fn invalid_patterns_say_why() {
        assert_eq!(Glob::new("[abc").unwrap_err(), GlobError::UnclosedClass);
        assert_eq!(Glob::new("[z-a]").unwrap_err(), GlobError::InvalidRange);
        assert_eq!(Glob::new("a}").unwrap_err(), GlobError::UnopenedAlternates);
        assert_eq!(Glob::new("{a,b").unwrap_err(), GlobError::UnclosedAlternates);
        assert_eq!(Glob::new("a\\").unwrap_err(), GlobError::DanglingEscape);
    }

    #[test]
    fn many_stars_stay_fast() {
        let g = Glob::new(&"*a".repeat(40)).unwrap();
        assert!(!g.is_match(&"a".repeat(39)));
        assert!(g.is_match(&"ba".repeat(60)));
    }
}
