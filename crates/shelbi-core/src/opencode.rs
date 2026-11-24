//! OpenCode facts shared by runner detection, the requirements check and
//! dispatch: which OpenCode Shelbi can drive and how to install or upgrade it.
//!
//! Shelbi drives OpenCode 2.x only. Its plugin uses the v2 module shape and
//! events, and the launch runs `opencode --standalone`, a v2 flag. On 1.x the
//! plugin silently does nothing, so an old install must read as unsupported
//! rather than as a working runner. The older npm package is still
//! 1.x; v2 ships as `@opencode/cli`.

use crate::ReasoningEffort;

/// The oldest OpenCode Shelbi drives (`major`, `minor`).
pub const OPENCODE_MIN_VERSION: (u32, u32) = (2, 3);

/// The command that installs OpenCode 2.x.
pub const OPENCODE_INSTALL: &str = "npm install -g @opencode/cli";

/// The command that moves an older OpenCode to the current release.
pub const OPENCODE_UPGRADE: &str = "opencode upgrade";

/// The `major.minor.patch` in `opencode --version` output: `opencode v2.4.1`
/// on 2.x, a bare `1.19.6` on 1.x. A missing patch reads as 0. `None` when
/// no token looks like a version.
pub fn parse_opencode_version(output: &str) -> Option<(u32, u32, u32)> {
    output.split_whitespace().find_map(|token| {
        let token = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
        let token = token.strip_prefix('v').unwrap_or(token);
        // Drop a pre-release or build suffix (`2.4.0-beta.1`).
        let core = token.split(['-', '+']).next()?;
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = match parts.next() {
            Some(p) => p.parse().ok()?,
            None => 0,
        };
        Some((major, minor, patch))
    })
}

/// Whether `opencode --version` output names a version Shelbi can drive.
/// `None` when the output carries no version (Shelbi then can't tell, and
/// callers let it through rather than refuse a working install).
pub fn opencode_version_supported(output: &str) -> Option<bool> {
    parse_opencode_version(output).map(|(major, minor, _)| (major, minor) >= OPENCODE_MIN_VERSION)
}

/// What Shelbi says about an OpenCode too old to drive: the same sentence in
/// the requirements check, new-project detection and a refused dispatch.
/// `version` is as the user would see it (`1.19.6`).
pub fn opencode_unsupported_message(version: &str) -> String {
    let (major, minor) = OPENCODE_MIN_VERSION;
    format!(
        "OpenCode {version} isn't supported. Shelbi needs OpenCode {major}.{minor} or later: \
         run `{OPENCODE_UPGRADE}`."
    )
}

/// The screen signatures a pane falls back to when Shelbi's OpenCode plugin
/// never reported in (no `plugin.alive` beacon after launch). Normally the
/// plugin marks a permission ask as `shelbi:blocked`; without it, the poller
/// reads the pane for OpenCode's own ask dialog ("△ Permission required").
/// The question form's wording isn't pinned yet, so it has no signature.
pub fn opencode_fallback_dialog_signatures() -> Vec<crate::DialogSignature> {
    vec![crate::DialogSignature::new("permission", "Permission required")]
}

/// One model `opencode models` lists: its `provider/model` id and the
/// reasoning variants OpenCode reports for it (`low`, `high`, `max`, …;
/// provider-specific, and often none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeModel {
    /// `provider/model`, as OpenCode's `model` config takes it.
    pub id: String,
    pub variants: Vec<String>,
}

/// The reasoning efforts a model with `variants` can take: each of Low,
/// Medium, High and Max whose same-named variant it reports, lowest first
/// (decision Q4, the same mapping as `shelbi_agent::opencode_effort_variant`
/// at launch).
/// Variants with no Shelbi effort (`minimal`, `xhigh`, `none`) aren't
/// offered. Empty: the model takes no effort Shelbi can set.
pub fn efforts_for_variants(variants: &[String]) -> Vec<ReasoningEffort> {
    [ReasoningEffort::Low, ReasoningEffort::Medium, ReasoningEffort::High, ReasoningEffort::Max]
        .into_iter()
        .filter(|e| variants.iter().any(|v| v.trim().eq_ignore_ascii_case(e.as_str())))
        .collect()
}

/// The ids in `opencode models` output: one `provider/model` per line.
/// Anything else (a blank line, a log line) is skipped.
pub fn parse_opencode_models(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(char::is_whitespace) && l.contains('/'))
        .map(str::to_string)
        .collect()
}

/// What Settings › Agents says an OpenCode agent runs with: how OpenCode is
/// launched, and every file and environment variable Shelbi adds. One
/// place, so the disclosure follows the launch (`shelbi_agent`'s
/// `opencode_launch_command`).
pub const OPENCODE_DISCLOSURE: &[&str] = &[
    "OpenCode runs in a private server per pane (opencode --standalone). Shelbi adds its instructions, \
     skills, plugin and permission rules through environment variables, and doesn't change your OpenCode config.",
    "Sessions: each task runs in its own session (--session ses_shelbi…), titled \"Shelbi · <workspace> · <task>\" \
     and archived when the task is done. Your own sessions in the worktree are left alone.",
    "Files: .shelbi/opencode/ in each workspace (config.json, instructions.md, skills/, Shelbi's plugin in \
     plugins/shelbi/, and its plugin.lock and plugin.alive), rewritten at each launch. Nothing is written to \
     ~/.config/opencode, .opencode/, opencode.json or AGENTS.md.",
    "Environment: OPENCODE_CONFIG_CONTENT (config.json, merged over your own value, which is kept in \
     SHELBI_OPENCODE_USER_CONFIG), OPENCODE_CLI_CONFIG_CONTENT (turns off OpenCode's terminal title), \
     OPENCODE_DISABLE_AUTOUPDATE=1, SHELBI_PANE_TTY, SHELBI_OPENCODE_SESSION and SHELBI_OPENCODE_TITLE.",
];

/// The note an OpenCode agent shows when no permission ceiling applies
/// (decision Q2): Shelbi imposes nothing, so OpenCode's own settings decide.
pub const OPENCODE_NO_CEILING_NOTE: &str = "OpenCode's own default allows edits and shell commands. \
     Your OpenCode config, or a permission ceiling set here or in the project, restricts them; \
     with neither, Shelbi imposes nothing.";

/// The id of the OpenCode session Shelbi launches for one task on one
/// workspace: `ses_shelbi` plus 20 hex characters hashed from the project,
/// workspace, task and generation.
///
/// The launch passes it as `--session <id>`, which reopens that session or
/// creates it, so a resume lands on exactly this conversation rather than on
/// whatever session in the worktree is newest (`--continue`), which could be
/// one the user started there. A fresh start bumps `generation` and so gets a
/// new session. The id is a pure function of its inputs, so it reads the same
/// on every host and across releases; the hash is FNV-1a (128-bit), spelled
/// out here rather than borrowed from a library whose output could change.
pub fn opencode_session_id(project: &str, workspace: &str, task: &str, generation: u32) -> String {
    const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013b;
    let mut hash = OFFSET;
    let generation = generation.to_string();
    // A NUL after each field keeps (`a b`, `c`) apart from (`a`, `b c`).
    for field in [project, workspace, task, generation.as_str()] {
        for byte in field.bytes().chain([0]) {
            hash ^= u128::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    // The top 80 bits, as 20 hex characters.
    format!("ses_shelbi{:020x}", hash >> 48)
}

/// The title Shelbi gives its OpenCode session, so the user can tell it
/// apart from their own in OpenCode's session list.
pub fn opencode_session_title(workspace: &str, task: &str) -> String {
    format!("Shelbi · {workspace} · {task}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_signatures_match_the_permission_dialog() {
        let sigs = opencode_fallback_dialog_signatures();
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].kind, "permission");
        assert!("  △ Permission required\n  Allow once".contains(&sigs[0].pattern));
    }

    #[test]
    fn versions_parse_from_both_major_lines() {
        assert_eq!(parse_opencode_version("opencode v2.4.1"), Some((2, 4, 1)));
        assert_eq!(parse_opencode_version("1.19.6\n"), Some((1, 19, 6)));
        assert_eq!(parse_opencode_version("v2.3"), Some((2, 3, 0)));
        assert_eq!(parse_opencode_version("opencode 2.5.0-beta.2"), Some((2, 5, 0)));
        assert_eq!(parse_opencode_version("opencode"), None);
        assert_eq!(parse_opencode_version(""), None);
    }

    #[test]
    fn the_gate_is_two_point_three() {
        assert_eq!(opencode_version_supported("1.19.6"), Some(false));
        assert_eq!(opencode_version_supported("opencode v2.2.9"), Some(false));
        assert_eq!(opencode_version_supported("opencode v2.3.0"), Some(true));
        assert_eq!(opencode_version_supported("opencode v2.4.1"), Some(true));
        assert_eq!(opencode_version_supported("opencode v3.0.0"), Some(true));
        assert_eq!(opencode_version_supported("garbled"), None);
    }

    #[test]
    fn session_ids_are_deterministic_and_distinct() {
        let id = opencode_session_id("shelbi", "bravo", "opencode-sessions", 0);
        assert_eq!(id, opencode_session_id("shelbi", "bravo", "opencode-sessions", 0));
        assert!(id.starts_with("ses_shelbi"), "{id}");
        assert_eq!(id.len(), "ses_shelbi".len() + 20, "{id}");
        assert!(id["ses_shelbi".len()..].chars().all(|c| c.is_ascii_hexdigit()), "{id}");
        // Every input moves the id, and fields can't bleed into each other.
        let others = [
            opencode_session_id("shelbi", "bravo", "opencode-sessions", 1),
            opencode_session_id("shelbi", "alpha", "opencode-sessions", 0),
            opencode_session_id("other", "bravo", "opencode-sessions", 0),
            opencode_session_id("shelbi", "bravo", "other-task", 0),
            opencode_session_id("shelbib", "ravo", "opencode-sessions", 0),
        ];
        for other in &others {
            assert_ne!(&id, other);
        }
    }

    /// The hash is spelled out in this crate, so an id never changes under a
    /// session that is still on disk. This pins it.
    #[test]
    fn session_ids_are_stable_across_releases() {
        assert_eq!(opencode_session_id("p", "w", "t", 0), PINNED_ID);
    }
    const PINNED_ID: &str = "ses_shelbi2249bbf7ae659ba8713f";

    #[test]
    fn session_title_names_the_workspace_and_task() {
        assert_eq!(opencode_session_title("bravo", "fix-it"), "Shelbi · bravo · fix-it");
    }

    #[test]
    fn the_message_names_the_version_and_the_upgrade() {
        assert_eq!(
            opencode_unsupported_message("1.19.6"),
            "OpenCode 1.19.6 isn't supported. Shelbi needs OpenCode 2.3 or later: run `opencode upgrade`."
        );
    }

    #[test]
    fn efforts_follow_the_variants_a_model_reports() {
        let v = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(
            efforts_for_variants(&v(&["low", "high", "max"])),
            [ReasoningEffort::Low, ReasoningEffort::High, ReasoningEffort::Max]
        );
        assert_eq!(
            efforts_for_variants(&v(&["minimal", "low", "medium", "high", "xhigh", "max"])),
            [ReasoningEffort::Low, ReasoningEffort::Medium, ReasoningEffort::High, ReasoningEffort::Max]
        );
        assert_eq!(efforts_for_variants(&v(&["none", "minimal", "xhigh"])), []);
        assert_eq!(efforts_for_variants(&[]), []);
        assert_eq!(efforts_for_variants(&v(&["HIGH"])), [ReasoningEffort::High]);
    }

    #[test]
    fn models_output_is_one_id_per_line() {
        let out = "opencode/big-pickle\n\nanthropic/claude-sonnet-5-5\nWARN something odd\n";
        assert_eq!(parse_opencode_models(out), ["opencode/big-pickle", "anthropic/claude-sonnet-5-5"]);
        assert!(parse_opencode_models("").is_empty());
    }
}
