//! OpenCode facts shared by runner detection, the requirements check and
//! dispatch: which OpenCode Shelbi can drive and how to install or upgrade it.
//!
//! Shelbi drives OpenCode 2.x only. Its plugin uses the v2 module shape and
//! events, and the launch runs `opencode --standalone`, a v2 flag. On 1.x the
//! plugin silently does nothing, so an old install must read as unsupported
//! rather than as a working runner. The older npm package is still
//! 1.x; v2 ships as `@opencode/cli`.

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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_message_names_the_version_and_the_upgrade() {
        assert_eq!(
            opencode_unsupported_message("1.19.6"),
            "OpenCode 1.19.6 isn't supported. Shelbi needs OpenCode 2.3 or later: run `opencode upgrade`."
        );
    }
}
