//! Pluggable agent CLI runners.
//!
//! v1 model: every runner is a CLI command + flags. shelbi launches it in
//! interactive mode inside a tmux pane and drives it with send-keys.
//!
//! In the future this trait can grow methods for richer integration
//! (`session_id`, `--resume`, streaming JSON), but the v1 surface stays
//! intentionally minimal.

use shelbi_core::AgentRunnerSpec;

/// POSIX shell-quoting, re-exported from `shelbi-core` so the historical
/// `shelbi_agent::shell_escape` path keeps working for the command-string
/// builders across the orchestrator and CLI. The canonical definition (and
/// its tests) live in [`shelbi_core::shell`]; the SSH-boundary escaper in
/// `shelbi-ssh` reaches the same function without depending on this crate.
pub use shelbi_core::shell_escape;

/// Construct the shell command to launch the agent CLI inside a tmux pane.
/// Returns a single string suitable for `tmux new-window -- <command>`.
pub fn launch_command(spec: &AgentRunnerSpec) -> String {
    let mut parts = vec![shell_escape(&spec.command)];
    for f in &spec.flags {
        parts.push(shell_escape(f));
    }
    parts.join(" ")
}

/// Whether `command` launches Claude Code. Keyed off the path basename so a
/// runner declared as `/usr/local/bin/claude` classifies the same as a bare
/// `claude`.
pub fn is_claude_runner(command: &str) -> bool {
    std::path::Path::new(command)
        .file_name()
        .and_then(|s| s.to_str())
        == Some("claude")
}

/// Whether `command` launches Codex. Keyed off the path basename so a runner
/// declared as `/opt/homebrew/bin/codex` classifies the same as a bare `codex`.
pub fn is_codex_runner(command: &str) -> bool {
    std::path::Path::new(command)
        .file_name()
        .and_then(|s| s.to_str())
        == Some("codex")
}

/// Return a copy of `spec` with `--permission-mode <mode>` appended when the
/// runner is `claude` and the mode is non-default. Passing the mode on the
/// command line is the authoritative signal; relying on `settings.json`'s
/// `defaultMode` is fragile (silent fallback to interactive on any I/O race
/// or version regression). For non-claude runners (and the `default` mode,
/// which is claude's own baseline) the spec is returned unchanged.
///
/// Idempotent: if the user-authored YAML already includes `--permission-mode`
/// in `flags` (common for projects that adopted the flag before this helper
/// existed), the spec is returned unchanged so the launched command line
/// doesn't end up with two copies. Two copies don't break claude — the
/// right-most wins — but they clutter pane captures and obscure which mode
/// the workspace is actually running in.
pub fn with_permission_mode(spec: &AgentRunnerSpec, mode: &str) -> AgentRunnerSpec {
    if !is_claude_runner(&spec.command) || mode == "default" {
        return spec.clone();
    }
    if spec
        .flags
        .iter()
        .any(|f| f == "--permission-mode" || f.starts_with("--permission-mode="))
    {
        return spec.clone();
    }
    let mut out = spec.clone();
    out.flags.push("--permission-mode".into());
    out.flags.push(mode.into());
    out
}

/// Return a copy of `spec` with `--continue` appended when the runner is
/// `claude` and `resume` is set. `claude --continue` reloads the most recent
/// conversation in the pane's working directory, so a resumed workspace picks
/// up mid-thought with its full prior context — the session transcript lives
/// under the user's `~/.claude/` and survives a killed tmux pane or a
/// recreated worktree (the cwd path is stable). This is the strongest resume
/// semantics shelbi can offer, so `shelbi task resume` prefers it for claude
/// and falls back to plain prompt re-injection for every other runner (which
/// has no equivalent transcript-resume flag shelbi knows how to drive).
///
/// Non-claude runners and `resume == false` return the spec unchanged.
/// Idempotent: a YAML that already carries `--continue` / `-c` / `--resume`
/// in `flags` isn't given a second copy.
pub fn with_continue(spec: &AgentRunnerSpec, resume: bool) -> AgentRunnerSpec {
    if !resume || !is_claude_runner(&spec.command) {
        return spec.clone();
    }
    if spec
        .flags
        .iter()
        .any(|f| f == "--continue" || f == "-c" || f == "--resume")
    {
        return spec.clone();
    }
    let mut out = spec.clone();
    out.flags.push("--continue".into());
    out
}

/// How a runner receives hub→workspace messages (a `shelbi message`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageChannel {
    /// Shelbi wires runner hooks it has verified are healthy on this runner,
    /// so the pane pushes new messages into the agent itself and the
    /// workspace prompt carries no polling contract.
    Hooks,
    /// No verified-healthy hook channel exists for this runner, so the
    /// workspace prompt must instruct the agent to tail
    /// `.shelbi/messages/<task-id>.log` itself on a concrete cadence and ack
    /// each line.
    Polling,
}

impl MessageChannel {
    /// Stable token for a dispatch event / status detail line.
    pub fn as_str(self) -> &'static str {
        match self {
            MessageChannel::Hooks => "hooks",
            MessageChannel::Polling => "polling",
        }
    }
}

/// Resolve which delivery channel a runner uses, keyed off a *verified hook
/// health* signal rather than the executable basename alone.
///
/// - **Claude** receives messages through hooks Shelbi wires in
///   `.claude/settings.json` — a Shelbi-owned config layer — and that push
///   path is the proven, shipped default, so claude resolves to
///   [`MessageChannel::Hooks`].
/// - **Codex and every other runner** resolve to [`MessageChannel::Polling`].
///   The installed Codex CLI rejects the `-c core.hooksPath=<path>` override
///   Shelbi previously passed (`unknown configuration field 'core'` under
///   strict validation), and Codex only discovers hooks from `~/.codex/` or
///   `<repo>/.codex/` (both user-owned, which Shelbi must not overwrite) or
///   plugin manifests. With no supported, non-destructive hook layer whose
///   handshake Shelbi has verified — and Codex `Stop` hooks additionally
///   requiring JSON output the shared drain script does not emit — there is no
///   verified-healthy hook channel to wire, so Codex polls until one is proven.
///
/// Keep this the single decision point: `polls_for_messages` and the
/// per-launch mode log both derive from it, so hooks and polling can never
/// disagree about a runner.
pub fn message_channel(spec: &AgentRunnerSpec) -> MessageChannel {
    if is_claude_runner(&spec.command) {
        MessageChannel::Hooks
    } else {
        MessageChannel::Polling
    }
}

/// Does this runner pull hub→workspace messages by polling the log itself?
///
/// The complement of a verified-healthy hook channel: any runner Shelbi has
/// not proven can push messages through hooks must be told to poll. See
/// [`message_channel`] for the health basis behind each runner's mode.
pub fn polls_for_messages(spec: &AgentRunnerSpec) -> bool {
    matches!(message_channel(spec), MessageChannel::Polling)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_command_minimal() {
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        assert_eq!(launch_command(&spec), "claude");
    }

    #[test]
    fn launch_command_with_flags() {
        let spec = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec!["--print".into(), "thinking".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        assert_eq!(launch_command(&spec), "codex --print thinking");
    }

    #[test]
    fn with_permission_mode_appends_for_claude() {
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
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
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = with_permission_mode(&spec, "acceptEdits");
        assert_eq!(
            out.flags,
            vec![
                "--dangerously-skip-permissions",
                "--permission-mode",
                "acceptEdits"
            ]
        );
    }

    #[test]
    fn with_permission_mode_resolves_absolute_claude_paths() {
        // A project might pin claude to /opt/homebrew/bin/claude; the helper
        // should still recognize it by basename.
        let spec = AgentRunnerSpec {
            command: "/opt/homebrew/bin/claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = with_permission_mode(&spec, "auto");
        assert_eq!(out.flags, vec!["--permission-mode", "auto"]);
    }

    #[test]
    fn with_permission_mode_skips_non_claude_runners() {
        let spec = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec!["--print".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = with_permission_mode(&spec, "auto");
        // Codex doesn't understand --permission-mode; leave it alone.
        assert_eq!(out.flags, vec!["--print"]);
    }

    #[test]
    fn with_permission_mode_skips_default_mode() {
        // `default` is claude's own baseline; passing the flag is redundant
        // and could surprise a user who reads the launched command line.
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = with_permission_mode(&spec, "default");
        assert!(out.flags.is_empty());
    }

    #[test]
    fn with_permission_mode_idempotent_when_yaml_already_has_flag() {
        // The shelbi project YAML kept `flags: [--permission-mode, auto]` as a
        // pre-bd7a23f quick fix; after bd7a23f added with_permission_mode the
        // spawn path was producing `claude --permission-mode auto
        // --permission-mode auto`. Detect the existing flag and skip the
        // append so the launched command line stays clean.
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--permission-mode".into(), "auto".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = with_permission_mode(&spec, "auto");
        assert_eq!(out.flags, vec!["--permission-mode", "auto"]);
        assert_eq!(launch_command(&out), "claude --permission-mode auto");
    }

    #[test]
    fn with_permission_mode_idempotent_for_equals_form() {
        // YAML may pin the single-token spelling (`--permission-mode=plan`).
        // The helper must recognize it too; otherwise it appends the
        // two-token form, the rightmost copy wins, and the workspace runs
        // in a mode the user explicitly configured away from.
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--permission-mode=plan".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = with_permission_mode(&spec, "auto");
        assert_eq!(out.flags, vec!["--permission-mode=plan"]);
    }

    #[test]
    fn with_permission_mode_idempotent_even_when_yaml_mode_differs() {
        // If the YAML pins a specific mode, respect it rather than silently
        // overriding from project.workspace_permissions_mode. An explicit flag
        // in the YAML is intentional configuration; quiet override would be
        // surprising.
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--permission-mode".into(), "plan".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = with_permission_mode(&spec, "auto");
        assert_eq!(out.flags, vec!["--permission-mode", "plan"]);
    }

    fn runner(command: &str) -> AgentRunnerSpec {
        AgentRunnerSpec {
            command: command.into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
        }
    }

    #[test]
    fn claude_does_not_poll_for_messages() {
        assert!(!polls_for_messages(&runner("claude")));
        assert!(!polls_for_messages(&runner("/usr/local/bin/claude")));
    }

    #[test]
    fn codex_polls_for_messages_without_a_verified_hook_channel() {
        // The installed Codex CLI rejects the `-c core.hooksPath` override and
        // exposes no non-destructive hook layer Shelbi can wire, so Codex must
        // fall back to prompt-level polling rather than be silently classified
        // hook-capable by basename.
        assert!(polls_for_messages(&runner("codex")));
        assert!(polls_for_messages(&runner("/opt/bin/codex")));
        assert_eq!(message_channel(&runner("codex")), MessageChannel::Polling);
    }

    #[test]
    fn claude_uses_the_hook_channel() {
        assert_eq!(message_channel(&runner("claude")), MessageChannel::Hooks);
        assert_eq!(
            message_channel(&runner("/usr/local/bin/claude")),
            MessageChannel::Hooks
        );
    }

    #[test]
    fn unknown_runners_poll_for_messages() {
        assert!(polls_for_messages(&runner("aider")));
        // No `.exe` special-casing: a Windows-style basename isn't "claude",
        // so it classifies as a polling runner — consistent with how
        // `with_permission_mode` keys off the exact basename.
        assert!(polls_for_messages(&runner("claude.exe")));
    }

    #[test]
    fn with_continue_appends_for_claude_when_resuming() {
        let out = with_continue(&runner("claude"), true);
        assert_eq!(out.flags, vec!["--continue"]);
        // Absolute claude path classifies the same.
        let out = with_continue(&runner("/usr/local/bin/claude"), true);
        assert_eq!(out.flags, vec!["--continue"]);
    }

    #[test]
    fn with_continue_is_noop_when_not_resuming() {
        let out = with_continue(&runner("claude"), false);
        assert!(out.flags.is_empty());
    }

    #[test]
    fn with_continue_skips_non_claude_runners() {
        // codex has no `--continue` shelbi knows how to drive; leave it alone.
        let out = with_continue(&runner("codex"), true);
        assert!(out.flags.is_empty());
    }

    #[test]
    fn with_continue_is_idempotent_when_flag_already_present() {
        for existing in ["--continue", "-c", "--resume"] {
            let spec = AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![existing.into()],
                prompt_injection: None,
                dialog_signatures: vec![],
            };
            let out = with_continue(&spec, true);
            assert_eq!(
                out.flags,
                vec![existing],
                "should not double-add for {existing}"
            );
        }
    }

    #[test]
    fn with_continue_preserves_existing_flags() {
        let spec = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--permission-mode".into(), "auto".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = with_continue(&spec, true);
        assert_eq!(out.flags, vec!["--permission-mode", "auto", "--continue"]);
    }
}
