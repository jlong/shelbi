//! What counts as a release for a project: the optional `releases:` block of
//! the project YAML.
//!
//! Each project follows its own release convention, so a release is whatever
//! git tag matches the project's glob. Shelbi tags `v*.*.*`; an iOS app that
//! ships through TestFlight might tag `testflight/<version>-<build>`:
//!
//! ```yaml
//! releases:
//!   tags: "v*.*.*"          # git tag glob in the project repo
//!   label: "{tag}"          # optional: {tag}, {version} placeholders
//! ```
//!
//! With no block there are no releases: nothing guesses a pattern. The
//! Changelog page draws each matching tag as a marker on its timeline
//! (`shelbi_state::changelog::releases` reads the tags).
//!
//! The glob is git's tag-list glob, minus its rarer corners: `*` matches any
//! run of characters (`/` included, as `git tag -l` does), `?` any one,
//! `[abc]` / `[a-z]` / `[!a-z]` a character class, and `\` escapes the next
//! character. A malformed one (an unclosed `[`, a trailing `\`, …) is a
//! config error ([`ReleasesConfig::validate`]), never a panic.

use serde::{Deserialize, Serialize};

/// The label a release gets when the block names none.
pub const DEFAULT_RELEASE_LABEL: &str = "{tag}";

/// A project's `releases:` block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasesConfig {
    /// The git tag glob a release's tag matches (`v*.*.*`).
    #[serde(default)]
    pub tags: String,
    /// How a release reads on the timeline: `{tag}` is the whole tag,
    /// `{version}` the tag without the glob's literal prefix (`v0.14.0` →
    /// `0.14.0` for `v*.*.*`). `None` is [`DEFAULT_RELEASE_LABEL`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ReleasesConfig {
    /// The parsed glob and a checked label template, or what's wrong with
    /// the block, naming the field (`releases.tags: …`).
    pub fn validate(&self) -> crate::Result<TagGlob> {
        let glob = TagGlob::parse(&self.tags)
            .map_err(|e| crate::Error::InvalidReleases(format!("releases.tags `{}`: {e}", self.tags)))?;
        check_label(self.label_template())
            .map_err(|e| crate::Error::InvalidReleases(format!("releases.label: {e}")))?;
        Ok(glob)
    }

    /// The label template in effect.
    pub fn label_template(&self) -> &str {
        self.label.as_deref().unwrap_or(DEFAULT_RELEASE_LABEL)
    }

    /// `tag`'s label: the template with `{tag}` and `{version}` filled.
    pub fn label_for(&self, glob: &TagGlob, tag: &str) -> String {
        self.label_template()
            .replace("{tag}", tag)
            .replace("{version}", glob.version(tag))
    }
}

/// Every `{name}` in a label template is one it knows.
fn check_label(template: &str) -> Result<(), String> {
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(format!("`{template}` has a `{{` with no `}}`"));
        };
        let name = &after[..close];
        if name != "tag" && name != "version" {
            return Err(format!(
                "unknown placeholder `{{{name}}}` in `{template}` (use {{tag}} or {{version}})"
            ));
        }
        rest = &after[close + 1..];
    }
    Ok(())
}

/// One piece of a parsed glob.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(char),
    /// `?`
    One,
    /// `*`
    Any,
    /// `[…]`: the ranges (a single character is a one-wide range), negated
    /// with `!` or `^`.
    Class { ranges: Vec<(char, char)>, negated: bool },
}

impl Token {
    /// Whether this single-character token matches `c` (never for `*`).
    fn matches(&self, c: char) -> bool {
        match self {
            Token::Literal(l) => *l == c,
            Token::One => true,
            Token::Any => false,
            Token::Class { ranges, negated } => {
                ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi) != *negated
            }
        }
    }
}

/// A parsed `releases.tags` glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagGlob {
    tokens: Vec<Token>,
    /// The literal text before the first wildcard (`v` for `v*.*.*`).
    prefix: String,
}

impl TagGlob {
    /// Parse `pattern`, or say what's wrong with it.
    pub fn parse(pattern: &str) -> Result<TagGlob, String> {
        if pattern.trim().is_empty() {
            return Err("the pattern is empty".into());
        }
        let mut tokens = Vec::new();
        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            tokens.push(match c {
                '*' => {
                    // `**` is `*`.
                    if tokens.last() == Some(&Token::Any) {
                        continue;
                    }
                    Token::Any
                }
                '?' => Token::One,
                '\\' => Token::Literal(chars.next().ok_or("it ends with a `\\` escaping nothing")?),
                '[' => {
                    let negated = chars.next_if(|&c| c == '!' || c == '^').is_some();
                    let mut ranges = Vec::new();
                    let mut closed = false;
                    // A `]` first in the class is a literal.
                    let mut first = true;
                    while let Some(c) = chars.next() {
                        if c == ']' && !first {
                            closed = true;
                            break;
                        }
                        first = false;
                        let lo = if c == '\\' {
                            chars.next().ok_or("it ends with a `\\` escaping nothing")?
                        } else {
                            c
                        };
                        let hi = if chars.peek() == Some(&'-') {
                            let mut ahead = chars.clone();
                            ahead.next();
                            match ahead.peek() {
                                // `[a-]`: the `-` is a literal.
                                Some(']') | None => lo,
                                Some(_) => {
                                    chars.next();
                                    let hi = match chars.next() {
                                        Some('\\') => chars.next().ok_or("it ends with a `\\` escaping nothing")?,
                                        Some(h) => h,
                                        None => break,
                                    };
                                    if hi < lo {
                                        return Err(format!("the range `{lo}-{hi}` runs backwards"));
                                    }
                                    hi
                                }
                            }
                        } else {
                            lo
                        };
                        ranges.push((lo, hi));
                    }
                    if !closed {
                        return Err("a `[` is never closed with `]`".into());
                    }
                    Token::Class { ranges, negated }
                }
                c => Token::Literal(c),
            });
        }
        let prefix = tokens
            .iter()
            .map_while(|t| match t {
                Token::Literal(c) => Some(*c),
                _ => None,
            })
            .collect();
        Ok(TagGlob { tokens, prefix })
    }

    /// The literal text before the first wildcard.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// `tag` without the glob's literal prefix: the `{version}` placeholder.
    pub fn version<'a>(&self, tag: &'a str) -> &'a str {
        tag.strip_prefix(self.prefix.as_str()).unwrap_or(tag)
    }

    /// Whether `tag` matches the whole glob.
    pub fn matches(&self, tag: &str) -> bool {
        let text: Vec<char> = tag.chars().collect();
        let (mut t, mut p) = (0, 0);
        // The last `*` seen and where in the text it began matching.
        let mut star: Option<(usize, usize)> = None;
        while t < text.len() {
            match self.tokens.get(p) {
                Some(Token::Any) => {
                    star = Some((p, t));
                    p += 1;
                }
                Some(tok) if tok.matches(text[t]) => {
                    t += 1;
                    p += 1;
                }
                _ => match star {
                    // Let the last `*` take one more character and retry.
                    Some((sp, st)) => {
                        star = Some((sp, st + 1));
                        p = sp + 1;
                        t = st + 1;
                    }
                    None => return false,
                },
            }
        }
        self.tokens[p..].iter().all(|tok| *tok == Token::Any)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(p: &str) -> TagGlob {
        TagGlob::parse(p).unwrap()
    }

    #[test]
    fn semver_tags_match_and_others_dont() {
        let g = glob("v*.*.*");
        for tag in ["v0.14.0", "v0.13.3", "v1.0.0-rc.1", "v10.20.30"] {
            assert!(g.matches(tag), "{tag}");
        }
        for tag in ["0.14.0", "v0.14", "v", "release-v0.1.0", "testflight/1.2.0-1"] {
            assert!(!g.matches(tag), "{tag}");
        }
        assert_eq!(g.prefix(), "v");
        assert_eq!(g.version("v0.14.0"), "0.14.0");
    }

    #[test]
    fn a_star_crosses_slashes_like_git_tag_list() {
        let g = glob("testflight/*");
        assert!(g.matches("testflight/1.2.0-1"));
        assert!(g.matches("testflight/a/b"));
        assert!(!g.matches("testflight"));
        assert!(!g.matches("appstore/1.2.0"));
        assert_eq!(g.version("testflight/1.2.0-1"), "1.2.0-1");
        assert!(glob("*").matches(""));
        assert!(glob("a**b").matches("axyzb"));
    }

    #[test]
    fn question_marks_classes_and_escapes() {
        assert!(glob("v?").matches("v1"));
        assert!(!glob("v?").matches("v12"));
        let g = glob("v[0-9].[!x]");
        assert!(g.matches("v3.y"));
        assert!(!g.matches("va.y"));
        assert!(!g.matches("v3.x"));
        assert!(glob("[]a]").matches("]"));
        assert!(glob("[a-]").matches("-"));
        assert!(glob("r\\*").matches("r*"));
        assert!(!glob("r\\*").matches("rx"));
        // A literal glob is its own prefix and matches only itself.
        let g = glob("stable");
        assert!(g.matches("stable") && !g.matches("stable2"));
        assert_eq!(g.prefix(), "stable");
    }

    #[test]
    fn malformed_globs_say_what_is_wrong() {
        for (p, why) in [
            ("", "empty"),
            ("  ", "empty"),
            ("v[0-9", "never closed"),
            ("v\\", "escaping nothing"),
            ("v[9-0]", "backwards"),
        ] {
            let err = TagGlob::parse(p).unwrap_err();
            assert!(err.contains(why), "{p:?}: {err}");
        }
        let cfg = ReleasesConfig {
            tags: "v[0-9".into(),
            label: None,
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.starts_with("releases.tags `v[0-9`: "), "{err}");
    }

    #[test]
    fn labels_fill_tag_and_version() {
        let cfg = ReleasesConfig {
            tags: "testflight/*".into(),
            label: Some("TestFlight {version}".into()),
        };
        let g = cfg.validate().unwrap();
        assert_eq!(cfg.label_for(&g, "testflight/1.2.0-1"), "TestFlight 1.2.0-1");
        let plain = ReleasesConfig {
            tags: "v*.*.*".into(),
            label: None,
        };
        let g = plain.validate().unwrap();
        assert_eq!(plain.label_for(&g, "v0.14.0"), "v0.14.0");
        let both = ReleasesConfig {
            tags: "v*".into(),
            label: Some("{tag} ({version})".into()),
        };
        let g = both.validate().unwrap();
        assert_eq!(both.label_for(&g, "v2"), "v2 (2)");
    }

    #[test]
    fn unknown_label_placeholders_are_config_errors() {
        for (label, why) in [("{name}", "unknown placeholder `{name}`"), ("v{tag", "no `}`")] {
            let cfg = ReleasesConfig {
                tags: "v*".into(),
                label: Some(label.into()),
            };
            let err = cfg.validate().unwrap_err().to_string();
            assert!(err.starts_with("releases.label: ") && err.contains(why), "{err}");
        }
    }

    #[test]
    fn the_block_parses_from_yaml() {
        let cfg: ReleasesConfig =
            yaml_serde::from_str("tags: \"testflight/*\"\nlabel: \"TestFlight {version}\"\n").unwrap();
        assert_eq!(cfg.tags, "testflight/*");
        assert_eq!(cfg.label.as_deref(), Some("TestFlight {version}"));
        let cfg: ReleasesConfig = yaml_serde::from_str("tags: \"v*.*.*\"\n").unwrap();
        assert_eq!(cfg.label, None);
        assert_eq!(cfg.label_template(), "{tag}");
    }
}
