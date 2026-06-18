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
}
