//! Shelbi-owned system plugin resolution.
//!
//! Release packages carry an editable copy of the plugin next to the binary.
//! The binary also embeds the exact same bundle. A parseable installed copy is
//! preferred so packaging defects are visible, while the embedded copy keeps
//! orchestrator startup functional when the package asset is absent or broken.

use std::path::{Path, PathBuf};

use serde_json::Value;

pub const SYSTEM_PLUGIN_NAME: &str = "update-shelbi-configuration";
pub const SYSTEM_SKILL_REL: &str = "skills/update-shelbi-configuration/SKILL.md";
pub(crate) const CLAUDE_MANIFEST_REL: &str = ".claude-plugin/plugin.json";
pub(crate) const CODEX_MANIFEST_REL: &str = ".codex-plugin/plugin.json";

pub(crate) const EMBEDDED_CLAUDE_MANIFEST: &str =
    include_str!("../../../plugins/update-shelbi-configuration/.claude-plugin/plugin.json");
pub(crate) const EMBEDDED_CODEX_MANIFEST: &str =
    include_str!("../../../plugins/update-shelbi-configuration/.codex-plugin/plugin.json");
const EMBEDDED_SKILL: &str = include_str!(
    "../../../plugins/update-shelbi-configuration/skills/update-shelbi-configuration/SKILL.md"
);

/// Stable compiled fingerprint of all files that affect the system skill.
///
/// FNV-1a is used as an integrity fingerprint, not as a security primitive.
/// The asset is deliberately still loaded when this differs: a package
/// maintainer may patch prose locally, and a valid patch must remain usable.
pub const EMBEDDED_CHECKSUM: u64 = bundle_checksum(
    EMBEDDED_CLAUDE_MANIFEST.as_bytes(),
    EMBEDDED_CODEX_MANIFEST.as_bytes(),
    EMBEDDED_SKILL.as_bytes(),
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    Installed,
    Embedded,
}

#[derive(Debug, Clone)]
pub struct ResolvedSystemPlugin {
    pub(crate) claude_manifest: String,
    pub(crate) codex_manifest: String,
    pub skill: String,
    pub source: PluginSource,
    pub warnings: Vec<String>,
}

impl ResolvedSystemPlugin {
    fn embedded(warning: String) -> Self {
        Self {
            claude_manifest: EMBEDDED_CLAUDE_MANIFEST.to_string(),
            codex_manifest: EMBEDDED_CODEX_MANIFEST.to_string(),
            skill: EMBEDDED_SKILL.to_string(),
            source: PluginSource::Embedded,
            warnings: vec![warning],
        }
    }
}

/// Resolve the packaged system plugin with an embedded fallback.
pub fn resolve_system_plugin() -> ResolvedSystemPlugin {
    let candidates = installed_plugin_candidates();
    if let Some(existing) = candidates.iter().find(|path| path.exists()) {
        return resolve_system_plugin_at(existing);
    }
    let display = candidates
        .first()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unresolved package path>".to_string());
    ResolvedSystemPlugin::embedded(format!(
        "system plugin `{SYSTEM_PLUGIN_NAME}` is missing at {display}; using embedded copy"
    ))
}

/// Resolve a specific package directory. Public within the crate so focused
/// tests can prove missing, malformed, modified, and valid behavior without
/// changing process-global executable state.
pub(crate) fn resolve_system_plugin_at(root: &Path) -> ResolvedSystemPlugin {
    let read = |rel: &str| {
        std::fs::read_to_string(root.join(rel))
            .map_err(|error| format!("could not read `{rel}`: {error}"))
    };
    let (claude, codex, skill) = match (
        read(CLAUDE_MANIFEST_REL),
        read(CODEX_MANIFEST_REL),
        read(SYSTEM_SKILL_REL),
    ) {
        (Ok(claude), Ok(codex), Ok(skill)) => (claude, codex, skill),
        (claude, codex, skill) => {
            let detail = claude
                .err()
                .or_else(|| codex.err())
                .or_else(|| skill.err())
                .unwrap_or_else(|| "asset is incomplete".to_string());
            return ResolvedSystemPlugin::embedded(format!(
                "system plugin `{SYSTEM_PLUGIN_NAME}` is missing or unreadable at {} ({detail}); using embedded copy",
                root.display()
            ));
        }
    };

    if let Err(detail) = validate_bundle(&claude, &codex, &skill) {
        return ResolvedSystemPlugin::embedded(format!(
            "system plugin `{SYSTEM_PLUGIN_NAME}` is malformed at {} ({detail}); using embedded copy",
            root.display()
        ));
    }

    let checksum = bundle_checksum(claude.as_bytes(), codex.as_bytes(), skill.as_bytes());
    let warnings = if checksum == EMBEDDED_CHECKSUM {
        Vec::new()
    } else {
        vec![format!(
            "system plugin `{SYSTEM_PLUGIN_NAME}` at {} differs from the compiled checksum; loading valid installed copy",
            root.display()
        )]
    };
    ResolvedSystemPlugin {
        claude_manifest: claude,
        codex_manifest: codex,
        skill,
        source: PluginSource::Installed,
        warnings,
    }
}

fn installed_plugin_candidates() -> Vec<PathBuf> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    installed_plugin_candidates_for_exe(&exe)
}

fn installed_plugin_candidates_for_exe(exe: &Path) -> Vec<PathBuf> {
    let Some(bin_dir) = exe.parent() else {
        return Vec::new();
    };
    let adjacent = bin_dir.join("plugins").join(SYSTEM_PLUGIN_NAME);
    let shared = bin_dir
        .parent()
        .unwrap_or(bin_dir)
        .join("share")
        .join("shelbi")
        .join("plugins")
        .join(SYSTEM_PLUGIN_NAME);

    // Package managers and the source installer place executables in a `bin`
    // directory and assets under the same prefix's `share`. Prefer that path
    // so an obsolete archive-style `bin/plugins` directory cannot shadow an
    // upgraded package. Standalone release archives have the binary and
    // `plugins` directory beside one another, so prefer the adjacent layout
    // everywhere else.
    let mut candidates = if bin_dir.file_name().is_some_and(|name| name == "bin") {
        vec![shared, adjacent]
    } else {
        vec![adjacent, shared]
    };
    // Cargo development builds live under <repo>/target/{debug,release}.
    // This is a compile-layout convenience, not an environment/user override.
    if bin_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "debug" || name == "release")
    {
        if let Some(repo) = bin_dir.parent().and_then(Path::parent) {
            candidates.push(repo.join("plugins").join(SYSTEM_PLUGIN_NAME));
        }
    }
    candidates
}

fn validate_bundle(claude: &str, codex: &str, skill: &str) -> Result<(), String> {
    let claude_skills = validate_manifest("Claude", claude)?;
    let codex_skills = validate_manifest("Codex", codex)?;
    if claude_skills != codex_skills {
        return Err("runner manifests reference different skill directories".to_string());
    }
    if claude_skills != "./skills/" {
        return Err("runner manifests must reference `./skills/`".to_string());
    }
    validate_skill(skill)
}

fn validate_manifest(runner: &str, text: &str) -> Result<String, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("{runner} manifest JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{runner} manifest root is not an object"))?;
    if object.get("name").and_then(Value::as_str) != Some(SYSTEM_PLUGIN_NAME) {
        return Err(format!(
            "{runner} manifest does not reserve name `{SYSTEM_PLUGIN_NAME}`"
        ));
    }
    object
        .get("skills")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{runner} manifest has no string `skills` path"))
}

fn validate_skill(skill: &str) -> Result<(), String> {
    let Some(frontmatter) = skill.strip_prefix("---\n") else {
        return Err("skill has no YAML frontmatter".to_string());
    };
    let Some((frontmatter, body)) = frontmatter.split_once("\n---\n") else {
        return Err("skill frontmatter is unterminated".to_string());
    };
    let name = frontmatter.lines().find_map(|line| {
        line.strip_prefix("name:")
            .map(str::trim)
            .filter(|name| !name.is_empty())
    });
    if name != Some(SYSTEM_PLUGIN_NAME) {
        return Err(format!(
            "skill frontmatter does not reserve name `{SYSTEM_PLUGIN_NAME}`"
        ));
    }
    if body.trim().is_empty() {
        return Err("skill body is empty".to_string());
    }
    Ok(())
}

const fn bundle_checksum(claude: &[u8], codex: &[u8], skill: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    hash = fnv_bytes(hash, CLAUDE_MANIFEST_REL.as_bytes());
    hash = fnv_bytes(hash, claude);
    hash = fnv_bytes(hash, CODEX_MANIFEST_REL.as_bytes());
    hash = fnv_bytes(hash, codex);
    hash = fnv_bytes(hash, SYSTEM_SKILL_REL.as_bytes());
    fnv_bytes(hash, skill)
}

const fn fnv_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        index += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_bundle(root: &Path, claude: &str, codex: &str, skill: &str) {
        for (rel, body) in [
            (CLAUDE_MANIFEST_REL, claude),
            (CODEX_MANIFEST_REL, codex),
            (SYSTEM_SKILL_REL, skill),
        ] {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
    }

    #[test]
    fn valid_modified_installed_asset_warns_and_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let modified = EMBEDDED_SKILL.replace(
            "# Update Shelbi configuration",
            "# Update Shelbi configuration\n\nInstalled package note.",
        );
        write_bundle(
            tmp.path(),
            EMBEDDED_CLAUDE_MANIFEST,
            EMBEDDED_CODEX_MANIFEST,
            &modified,
        );

        let resolved = resolve_system_plugin_at(tmp.path());
        assert_eq!(resolved.source, PluginSource::Installed);
        assert_eq!(resolved.skill, modified);
        assert_eq!(resolved.warnings.len(), 1);
        assert!(resolved.warnings[0].contains("differs from the compiled checksum"));
    }

    #[test]
    fn matching_installed_asset_loads_without_warning() {
        let tmp = tempfile::tempdir().unwrap();
        write_bundle(
            tmp.path(),
            EMBEDDED_CLAUDE_MANIFEST,
            EMBEDDED_CODEX_MANIFEST,
            EMBEDDED_SKILL,
        );

        let resolved = resolve_system_plugin_at(tmp.path());
        assert_eq!(resolved.source, PluginSource::Installed);
        assert_eq!(resolved.skill, EMBEDDED_SKILL);
        assert!(resolved.warnings.is_empty());
    }

    #[test]
    fn missing_and_malformed_assets_use_embedded_copy() {
        let missing = tempfile::tempdir().unwrap();
        let resolved = resolve_system_plugin_at(missing.path());
        assert_eq!(resolved.source, PluginSource::Embedded);
        assert_eq!(resolved.skill, EMBEDDED_SKILL);
        assert!(resolved.warnings[0].contains("missing or unreadable"));

        let malformed = tempfile::tempdir().unwrap();
        write_bundle(
            malformed.path(),
            "{}",
            EMBEDDED_CODEX_MANIFEST,
            EMBEDDED_SKILL,
        );
        let resolved = resolve_system_plugin_at(malformed.path());
        assert_eq!(resolved.source, PluginSource::Embedded);
        assert_eq!(resolved.skill, EMBEDDED_SKILL);
        assert!(resolved.warnings[0].contains("malformed"));
    }

    #[test]
    fn embedded_manifests_share_the_reserved_skill_directory() {
        validate_bundle(
            EMBEDDED_CLAUDE_MANIFEST,
            EMBEDDED_CODEX_MANIFEST,
            EMBEDDED_SKILL,
        )
        .unwrap();
    }

    #[test]
    fn release_archive_resolves_plugins_beside_binary() {
        let candidates =
            installed_plugin_candidates_for_exe(Path::new("/tmp/shelbi-release/shelbi"));
        assert_eq!(
            candidates[0],
            Path::new("/tmp/shelbi-release/plugins").join(SYSTEM_PLUGIN_NAME)
        );
    }

    #[test]
    fn linux_package_resolves_shared_data_before_adjacent_stale_copy() {
        let candidates = installed_plugin_candidates_for_exe(Path::new("/usr/bin/shelbi"));
        assert_eq!(
            candidates,
            [
                Path::new("/usr/share/shelbi/plugins").join(SYSTEM_PLUGIN_NAME),
                Path::new("/usr/bin/plugins").join(SYSTEM_PLUGIN_NAME),
            ]
        );
    }

    #[test]
    fn homebrew_resolves_formula_versioned_pkgshare() {
        let candidates = installed_plugin_candidates_for_exe(Path::new(
            "/opt/homebrew/Cellar/shelbi/0.6.0/bin/shelbi",
        ));
        assert_eq!(
            candidates[0],
            Path::new("/opt/homebrew/Cellar/shelbi/0.6.0/share/shelbi/plugins")
                .join(SYSTEM_PLUGIN_NAME)
        );
    }

    #[test]
    fn source_installer_resolves_custom_prefix_share() {
        let candidates = installed_plugin_candidates_for_exe(Path::new("/srv/shelbi/bin/shelbi"));
        assert_eq!(
            candidates[0],
            Path::new("/srv/shelbi/share/shelbi/plugins").join(SYSTEM_PLUGIN_NAME)
        );
    }
}
