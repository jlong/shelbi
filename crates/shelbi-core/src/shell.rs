//! Shell-quoting primitives shared across the shelbi workspace.
//!
//! Lives in `shelbi-core` (rather than a higher-level crate) so both the
//! process-boundary layer (`shelbi-ssh`, which escapes every argv element
//! it hands to a remote shell) and the command-string builders
//! (`shelbi-agent`, the orchestrator) can reach the same escaper without
//! anyone depending upward. See F2 in Shelbi ContextStore
//! docs/planning:reviews/adversarial-2026-07/process-boundaries.md.

/// Conservative POSIX-shell quoting: wrap in single quotes, escape internal
/// single quotes by closing-and-reopening (`'\''`).
///
/// Strings made up entirely of "obviously safe" characters
/// (`[A-Za-z0-9._/:=-]`, none of which are shell metacharacters) pass
/// through unquoted — this keeps the common case (`tmux`, `--flag`,
/// `path/to/thing`) readable on the wire and lets the SSH-boundary escaper
/// leave already-safe argv untouched. The empty string is quoted (`''`) so
/// it survives as a distinct, empty argument.
pub fn shell_escape(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn escape_simple_passes_through() {
        assert_eq!(shell_escape("claude"), "claude");
        assert_eq!(shell_escape("--print"), "--print");
        assert_eq!(shell_escape("path/to/thing"), "path/to/thing");
        assert_eq!(shell_escape("host:1.2.3=x"), "host:1.2.3=x");
    }

    #[test]
    fn escape_with_spaces() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
    }

    #[test]
    fn escape_with_quote() {
        assert_eq!(shell_escape("can't"), "'can'\\''t'");
    }

    #[test]
    fn escape_empty_string_is_quoted() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn escape_metacharacters_are_quoted() {
        // The exact class F2 cares about: comment marker, expansion, and
        // command separators must all be neutralized.
        assert_eq!(shell_escape("#{pane_title}"), "'#{pane_title}'");
        assert_eq!(shell_escape("$HOME"), "'$HOME'");
        assert_eq!(shell_escape("a && b; c"), "'a && b; c'");
    }

    /// Round-trip every metacharacter class through a real `sh -c` — the
    /// moral equivalent of the remote shell an SSH-routed command lands in.
    /// `printf %s` echoes the argument back untouched, so byte-for-byte
    /// equality proves the quoting survives shell re-parsing.
    #[test]
    fn escaped_arg_round_trips_through_sh() {
        for raw in [
            "hello world",
            "#{pane_title}",
            "cd /work/wt && claude",
            "a; b | c",
            "$HOME/x*y",
            "it's a \"trap\"",
            "",
        ] {
            let line = format!("printf %s {}", shell_escape(raw));
            let out = Command::new("sh")
                .arg("-c")
                .arg(&line)
                .output()
                .expect("sh -c failed to run");
            assert!(out.status.success(), "sh exited nonzero for {raw:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                raw,
                "round-trip mismatch for {raw:?} (wire: {line})"
            );
        }
    }
}
