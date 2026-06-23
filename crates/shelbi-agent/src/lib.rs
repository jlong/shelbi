//! Pluggable agent CLI runners.
//!
//! v1 model: every runner is a CLI command + flags. shelbi launches it in
//! interactive mode inside a tmux pane and drives it with send-keys.
//!
//! In the future this trait can grow methods for richer integration
//! (`session_id`, `--resume`, streaming JSON), but the v1 surface stays
//! intentionally minimal.

use shelbi_core::AgentRunnerSpec;

/// Construct the shell command to launch the agent CLI inside a tmux pane.
/// Returns a single string suitable for `tmux new-window -- <command>`.
pub fn launch_command(spec: &AgentRunnerSpec) -> String {
    let mut parts = vec![shell_escape(&spec.command)];
    for f in &spec.flags {
        parts.push(shell_escape(f));
    }
    parts.join(" ")
}

/// Return a copy of `spec` with `--permission-mode <mode>` appended when the
/// runner is `claude` and the mode is non-default. Passing the mode on the
/// command line is the authoritative signal; relying on `settings.json`'s
/// `defaultMode` is fragile (silent fallback to interactive on any I/O race
/// or version regression). For non-claude runners (and the `default` mode,
/// which is claude's own baseline) the spec is returned unchanged.
pub fn with_permission_mode(spec: &AgentRunnerSpec, mode: &str) -> AgentRunnerSpec {
    let is_claude = std::path::Path::new(&spec.command)
        .file_name()
        .and_then(|s| s.to_str())
        == Some("claude");
    if !is_claude || mode == "default" {
        return spec.clone();
    }
    let mut out = spec.clone();
    out.flags.push("--permission-mode".into());
    out.flags.push(mode.into());
    out
}

/// Conservative POSIX-shell quoting: wrap in single quotes, escape internal
/// single quotes by closing-and-reopening.
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

    #[test]
    fn escape_simple() {
        assert_eq!(shell_escape("claude"), "claude");
        assert_eq!(shell_escape("--print"), "--print");
        assert_eq!(shell_escape("path/to/thing"), "path/to/thing");
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
    fn launch_command_minimal() {
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
        };
        assert_eq!(launch_command(&spec), "claude");
    }

    #[test]
    fn launch_command_with_flags() {
        let spec = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec!["--print".into(), "thinking".into()],
        };
        assert_eq!(launch_command(&spec), "codex --print thinking");
    }

    #[test]
    fn with_permission_mode_appends_for_claude() {
        let spec = AgentRunnerSpec { command: "claude".into(), flags: vec![] };
        let out = with_permission_mode(&spec, "auto");
        assert_eq!(out.command, "claude");
        assert_eq!(out.flags, vec!["--permission-mode", "auto"]);
        assert_eq!(launch_command(&out), "claude --permission-mode auto");
    }

    #[test]
    fn with_permission_mode_preserves_existing_flags() {
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--dangerously-skip-permissions".into()],
        };
        let out = with_permission_mode(&spec, "acceptEdits");
        assert_eq!(
            out.flags,
            vec!["--dangerously-skip-permissions", "--permission-mode", "acceptEdits"]
        );
    }

    #[test]
    fn with_permission_mode_resolves_absolute_claude_paths() {
        // A project might pin claude to /opt/homebrew/bin/claude; the helper
        // should still recognize it by basename.
        let spec = AgentRunnerSpec {
            command: "/opt/homebrew/bin/claude".into(),
            flags: vec![],
        };
        let out = with_permission_mode(&spec, "auto");
        assert_eq!(out.flags, vec!["--permission-mode", "auto"]);
    }

    #[test]
    fn with_permission_mode_skips_non_claude_runners() {
        let spec = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec!["--print".into()],
        };
        let out = with_permission_mode(&spec, "auto");
        // Codex doesn't understand --permission-mode; leave it alone.
        assert_eq!(out.flags, vec!["--print"]);
    }

    #[test]
    fn with_permission_mode_skips_default_mode() {
        // `default` is claude's own baseline; passing the flag is redundant
        // and could surprise a user who reads the launched command line.
        let spec = AgentRunnerSpec { command: "claude".into(), flags: vec![] };
        let out = with_permission_mode(&spec, "default");
        assert!(out.flags.is_empty());
    }
}
