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
///
/// A *leading* `=` forces quoting even though `=` is safe mid-word: zsh's
/// `EQUALS` option (on by default, and zsh is the login shell on macOS and
/// many devboxes) expands a word-initial `=` as `=command` filename
/// expansion, so an unquoted exact-match tmux target like
/// `=shelbi-w-bob:agent` dies with "command not found" on the remote side.
pub fn shell_escape(s: &str) -> String {
    if !s.is_empty()
        && !s.starts_with('=')
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
    fn escape_leading_equals_is_quoted() {
        // Exact-match tmux targets (`={session}`) must be quoted or zsh's
        // EQUALS expansion mangles them; mid-word `=` stays pass-through.
        assert_eq!(
            shell_escape("=shelbi-w-bob:agent"),
            "'=shelbi-w-bob:agent'"
        );
        assert_eq!(shell_escape("="), "'='");
        assert_eq!(shell_escape("--format=short"), "--format=short");
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
            "=shelbi-w-bob:agent",
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

    /// Same round-trip through a real `zsh -c` — the shell that actually
    /// bit us: with the default EQUALS option, an unquoted word-initial `=`
    /// expands as `=command` and aborts the whole command line. Skipped when
    /// zsh isn't installed (e.g. minimal Linux CI images).
    #[test]
    fn escaped_arg_round_trips_through_zsh() {
        for raw in ["=shelbi-w-bob:agent", "=", "shelbi-myapp:=w-x", "$HOME/x*y"] {
            let line = format!("printf %s {}", shell_escape(raw));
            let out = match Command::new("zsh").arg("-c").arg(&line).output() {
                Ok(out) => out,
                Err(_) => return, // no zsh on this host
            };
            assert!(
                out.status.success(),
                "zsh exited nonzero for {raw:?} (wire: {line}, stderr: {})",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                raw,
                "round-trip mismatch for {raw:?} (wire: {line})"
            );
        }
    }
}
