//! The `shelbi-docs` system plugin: the skill that teaches every agent the
//! project's docs (`plugins/shelbi-docs/`).
//!
//! Like the configuration plugin ([`crate::system_plugin`]), release packages
//! carry an editable copy beside the binary and the binary embeds the same
//! bundle. A valid installed copy wins so packaging defects stay visible; the
//! embedded copy keeps the skill available when the package asset is missing
//! or broken.
//!
//! Unlike the configuration plugin, which only the orchestrator gets, this
//! skill goes to every agent, orchestrator and workers alike, on every runner:
//! [`crate::workspace`] mounts it into the worktree's `.claude/skills/`
//! (Claude Code), `.agents/skills/` (Codex) and, for an OpenCode worker,
//! `.shelbi/opencode/skills/`.

use std::path::Path;

use serde_json::Value;

use crate::system_plugin::PluginSource;

pub const DOCS_PLUGIN_NAME: &str = "shelbi-docs";

/// The plugin's name before Documentation was called that. Shelbi removes
/// the skill copies it mounted under this name, and an install carries no
/// plugin of it.
pub const LEGACY_DOCS_PLUGIN_NAME: &str = "shelbi-wiki";
pub(crate) const CLAUDE_MANIFEST_REL: &str = ".claude-plugin/plugin.json";
pub(crate) const CODEX_MANIFEST_REL: &str = ".codex-plugin/plugin.json";
pub const DOCS_SKILL_REL: &str = "skills/shelbi-docs/SKILL.md";

/// Every file in the bundle, as `(relative path, embedded body)`.
pub(crate) const EMBEDDED_FILES: [(&str, &str); 3] = [
    (
        CLAUDE_MANIFEST_REL,
        include_str!("../../../plugins/shelbi-docs/.claude-plugin/plugin.json"),
    ),
    (
        CODEX_MANIFEST_REL,
        include_str!("../../../plugins/shelbi-docs/.codex-plugin/plugin.json"),
    ),
    (
        DOCS_SKILL_REL,
        include_str!("../../../plugins/shelbi-docs/skills/shelbi-docs/SKILL.md"),
    ),
];

#[derive(Debug, Clone)]
pub struct ResolvedDocsPlugin {
    /// `(relative path, body)` for every bundle file, in [`EMBEDDED_FILES`] order.
    pub files: Vec<(&'static str, String)>,
    pub source: PluginSource,
    pub warnings: Vec<String>,
}

impl ResolvedDocsPlugin {
    fn embedded(warnings: Vec<String>) -> Self {
        Self {
            files: EMBEDDED_FILES
                .iter()
                .map(|(rel, body)| (*rel, (*body).to_string()))
                .collect(),
            source: PluginSource::Embedded,
            warnings,
        }
    }

    /// The skill's `SKILL.md`.
    pub fn skill(&self) -> &str {
        body(&self.files, DOCS_SKILL_REL)
    }
}

/// Resolve the packaged plugin, falling back to the embedded copy.
pub fn resolve_docs_plugin() -> ResolvedDocsPlugin {
    let candidates = crate::system_plugin::installed_candidates(DOCS_PLUGIN_NAME);
    match candidates.iter().find(|path| path.exists()) {
        Some(root) => resolve_docs_plugin_at(root),
        // Older packages predate this plugin: the embedded copy is the
        // expected source there, not a defect worth a warning.
        None => ResolvedDocsPlugin::embedded(Vec::new()),
    }
}

pub(crate) fn resolve_docs_plugin_at(root: &Path) -> ResolvedDocsPlugin {
    let mut files = Vec::new();
    for (rel, _) in EMBEDDED_FILES {
        match std::fs::read_to_string(root.join(rel)) {
            Ok(body) => files.push((rel, body)),
            Err(error) => {
                return ResolvedDocsPlugin::embedded(vec![format!(
                    "system plugin `{DOCS_PLUGIN_NAME}` is missing or unreadable at {} \
                     (could not read `{rel}`: {error}); using embedded copy",
                    root.display()
                )]);
            }
        }
    }
    if let Err(detail) = validate_bundle(&files) {
        return ResolvedDocsPlugin::embedded(vec![format!(
            "system plugin `{DOCS_PLUGIN_NAME}` is malformed at {} ({detail}); using \
             embedded copy",
            root.display()
        )]);
    }
    let differs = files
        .iter()
        .zip(EMBEDDED_FILES)
        .any(|((_, installed), (_, embedded))| installed != embedded);
    let warnings = if differs {
        vec![format!(
            "system plugin `{DOCS_PLUGIN_NAME}` at {} differs from the compiled copy; \
             loading valid installed copy",
            root.display()
        )]
    } else {
        Vec::new()
    };
    ResolvedDocsPlugin {
        files,
        source: PluginSource::Installed,
        warnings,
    }
}

fn body<'a>(files: &'a [(&'static str, String)], rel: &str) -> &'a str {
    files
        .iter()
        .find(|(r, _)| *r == rel)
        .map(|(_, body)| body.as_str())
        .unwrap_or("")
}

fn validate_bundle(files: &[(&'static str, String)]) -> Result<(), String> {
    for (runner, rel) in [("Claude", CLAUDE_MANIFEST_REL), ("Codex", CODEX_MANIFEST_REL)] {
        let manifest: Value = serde_json::from_str(body(files, rel))
            .map_err(|error| format!("{runner} manifest JSON: {error}"))?;
        if manifest.get("name").and_then(Value::as_str) != Some(DOCS_PLUGIN_NAME) {
            return Err(format!("{runner} manifest does not reserve name `{DOCS_PLUGIN_NAME}`"));
        }
        if manifest.get("skills").and_then(Value::as_str) != Some("./skills/") {
            return Err(format!("{runner} manifest must reference `./skills/`"));
        }
    }
    validate_skill(body(files, DOCS_SKILL_REL))
}

fn validate_skill(skill: &str) -> Result<(), String> {
    let frontmatter = skill
        .strip_prefix("---\n")
        .ok_or("skill has no YAML frontmatter")?;
    let (frontmatter, body) = frontmatter
        .split_once("\n---\n")
        .ok_or("skill frontmatter is unterminated")?;
    let field = |key: &str| {
        frontmatter.lines().find_map(|line| {
            line.strip_prefix(key)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
    };
    if field("name:") != Some(DOCS_PLUGIN_NAME) {
        return Err(format!("skill frontmatter does not reserve name `{DOCS_PLUGIN_NAME}`"));
    }
    if field("description:").is_none() {
        return Err("skill frontmatter has no description".to_string());
    }
    if body.trim().is_empty() {
        return Err("skill body is empty".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_bundle(root: &Path, files: &[(&str, String)]) {
        for (rel, body) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
    }

    fn embedded() -> Vec<(&'static str, String)> {
        EMBEDDED_FILES.iter().map(|(r, b)| (*r, (*b).to_string())).collect()
    }

    #[test]
    fn embedded_bundle_is_valid() {
        validate_bundle(&embedded()).unwrap();
    }

    /// The skill teaches the commands the CLI actually has, so a renamed or
    /// dropped subcommand breaks here instead of in an agent's session.
    #[test]
    fn embedded_skill_names_the_core_commands() {
        let skill = ResolvedDocsPlugin::embedded(Vec::new()).skill().to_string();
        for command in [
            "shelbi docs tree",
            "shelbi docs ls",
            "shelbi docs read",
            "shelbi docs cat",
            "shelbi docs search",
            "shelbi docs new",
            "shelbi docs write",
            "shelbi docs append",
            "shelbi docs mv",
            "shelbi docs rm",
            "shelbi docs tasks",
            "shelbi docs check",
            "shelbi docs sync",
        ] {
            assert!(skill.contains(command), "skill teaches `{command}`");
        }
        assert!(skill.contains("never in a repo file"), "plans go in the docs");
    }

    #[test]
    fn matching_installed_copy_loads_without_warning() {
        let tmp = tempfile::tempdir().unwrap();
        write_bundle(tmp.path(), &embedded());
        let resolved = resolve_docs_plugin_at(tmp.path());
        assert_eq!(resolved.source, PluginSource::Installed);
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    }

    #[test]
    fn modified_valid_copy_loads_with_a_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let mut files = embedded();
        files[2].1.push_str("\nPackager note.\n");
        write_bundle(tmp.path(), &files);
        let resolved = resolve_docs_plugin_at(tmp.path());
        assert_eq!(resolved.source, PluginSource::Installed);
        assert!(resolved.skill().contains("Packager note."));
        assert!(resolved.warnings[0].contains("differs from the compiled copy"));
    }

    #[test]
    fn missing_or_malformed_copy_falls_back_to_embedded() {
        let missing = tempfile::tempdir().unwrap();
        let resolved = resolve_docs_plugin_at(missing.path());
        assert_eq!(resolved.source, PluginSource::Embedded);
        assert!(resolved.warnings[0].contains("missing or unreadable"));

        let malformed = tempfile::tempdir().unwrap();
        let mut files = embedded();
        files[2].1 = "---\nname: something-else\n---\nBody\n".into();
        write_bundle(malformed.path(), &files);
        let resolved = resolve_docs_plugin_at(malformed.path());
        assert_eq!(resolved.source, PluginSource::Embedded);
        assert!(resolved.warnings[0].contains("malformed"), "{:?}", resolved.warnings);
        assert_eq!(resolved.skill(), EMBEDDED_FILES[2].1);
    }
}
