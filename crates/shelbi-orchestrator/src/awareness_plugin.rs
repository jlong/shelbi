//! The `shelbi-awareness` system plugin: awareness (v2) event delivery for a
//! Claude orchestrator (`plugins/shelbi-awareness/`).
//!
//! Like the configuration plugin ([`crate::system_plugin`]), release packages
//! carry an editable copy beside the binary and the binary embeds the same
//! bundle. A valid installed copy wins so packaging defects stay visible; the
//! embedded copy keeps delivery working when the package asset is missing or
//! broken. It is staged and passed with `--plugin-dir` only for a project on
//! `orchestrator.awareness: awareness`.

use std::path::Path;

use serde_json::Value;

use crate::system_plugin::PluginSource;

pub const AWARENESS_PLUGIN_NAME: &str = "shelbi-awareness";
pub(crate) const MANIFEST_REL: &str = ".claude-plugin/plugin.json";
pub(crate) const HOOKS_REL: &str = "hooks/hooks.json";
pub(crate) const MONITORS_REL: &str = "monitors/monitors.json";
pub(crate) const README_REL: &str = "README.md";

/// Every file in the bundle, as `(relative path, embedded body)`.
pub(crate) const EMBEDDED_FILES: [(&str, &str); 4] = [
    (
        MANIFEST_REL,
        include_str!("../../../plugins/shelbi-awareness/.claude-plugin/plugin.json"),
    ),
    (
        HOOKS_REL,
        include_str!("../../../plugins/shelbi-awareness/hooks/hooks.json"),
    ),
    (
        MONITORS_REL,
        include_str!("../../../plugins/shelbi-awareness/monitors/monitors.json"),
    ),
    (
        README_REL,
        include_str!("../../../plugins/shelbi-awareness/README.md"),
    ),
];

#[derive(Debug, Clone)]
pub struct ResolvedAwarenessPlugin {
    /// `(relative path, body)` for every bundle file, in [`EMBEDDED_FILES`] order.
    pub files: Vec<(&'static str, String)>,
    pub source: PluginSource,
    pub warnings: Vec<String>,
}

impl ResolvedAwarenessPlugin {
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
}

/// Resolve the packaged plugin, falling back to the embedded copy.
pub fn resolve_awareness_plugin() -> ResolvedAwarenessPlugin {
    let candidates = crate::system_plugin::installed_candidates(AWARENESS_PLUGIN_NAME);
    match candidates.iter().find(|path| path.exists()) {
        Some(root) => resolve_awareness_plugin_at(root),
        // Older packages predate this plugin: the embedded copy is the
        // expected source there, not a defect worth a warning.
        None => ResolvedAwarenessPlugin::embedded(Vec::new()),
    }
}

pub(crate) fn resolve_awareness_plugin_at(root: &Path) -> ResolvedAwarenessPlugin {
    let mut files = Vec::new();
    for (rel, _) in EMBEDDED_FILES {
        match std::fs::read_to_string(root.join(rel)) {
            Ok(body) => files.push((rel, body)),
            Err(error) => {
                return ResolvedAwarenessPlugin::embedded(vec![format!(
                    "system plugin `{AWARENESS_PLUGIN_NAME}` is missing or unreadable at {} \
                     (could not read `{rel}`: {error}); using embedded copy",
                    root.display()
                )]);
            }
        }
    }
    if let Err(detail) = validate_bundle(&files) {
        return ResolvedAwarenessPlugin::embedded(vec![format!(
            "system plugin `{AWARENESS_PLUGIN_NAME}` is malformed at {} ({detail}); using \
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
            "system plugin `{AWARENESS_PLUGIN_NAME}` at {} differs from the compiled copy; \
             loading valid installed copy",
            root.display()
        )]
    } else {
        Vec::new()
    };
    ResolvedAwarenessPlugin {
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

fn parse(files: &[(&'static str, String)], rel: &str) -> Result<Value, String> {
    serde_json::from_str(body(files, rel)).map_err(|error| format!("`{rel}`: {error}"))
}

fn validate_bundle(files: &[(&'static str, String)]) -> Result<(), String> {
    let manifest = parse(files, MANIFEST_REL)?;
    if manifest.get("name").and_then(Value::as_str) != Some(AWARENESS_PLUGIN_NAME) {
        return Err(format!("manifest does not reserve name `{AWARENESS_PLUGIN_NAME}`"));
    }
    let monitors = parse(files, MONITORS_REL)?;
    let monitors = monitors
        .as_array()
        .filter(|entries| !entries.is_empty())
        .ok_or("`monitors/monitors.json` is not a non-empty array")?;
    for entry in monitors {
        for key in ["name", "command", "description"] {
            if entry.get(key).and_then(Value::as_str).is_none() {
                return Err(format!("a monitor has no string `{key}`"));
            }
        }
    }
    let hooks = parse(files, HOOKS_REL)?;
    let events = hooks
        .get("hooks")
        .and_then(Value::as_object)
        .ok_or("`hooks/hooks.json` has no `hooks` object")?;
    for event in ["UserPromptSubmit", "Stop", "SessionStart"] {
        if !events.contains_key(event) {
            return Err(format!("`hooks/hooks.json` does not wire `{event}`"));
        }
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
    fn staging_follows_the_mode_and_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let host = shelbi_core::Host::Local;
        let root = tmp.path().join(crate::workspace::ORCHESTRATOR_AWARENESS_PLUGIN_REL);

        assert!(crate::workspace::deploy_orchestrator_awareness_plugin(&host, tmp.path(), true).unwrap());
        for (rel, _) in EMBEDDED_FILES {
            assert!(root.join(rel).is_file(), "{rel} staged");
        }
        std::fs::write(root.join("stale.json"), "{}").unwrap();

        // Re-staging starts clean; switching back to v1 removes the plugin.
        assert!(crate::workspace::deploy_orchestrator_awareness_plugin(&host, tmp.path(), true).unwrap());
        assert!(!root.join("stale.json").exists());
        assert!(!crate::workspace::deploy_orchestrator_awareness_plugin(&host, tmp.path(), false).unwrap());
        assert!(!root.exists());
    }

    #[test]
    fn embedded_bundle_is_valid() {
        validate_bundle(&embedded()).unwrap();
    }

    #[test]
    fn matching_installed_copy_loads_without_warning() {
        let tmp = tempfile::tempdir().unwrap();
        write_bundle(tmp.path(), &embedded());
        let resolved = resolve_awareness_plugin_at(tmp.path());
        assert_eq!(resolved.source, PluginSource::Installed);
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    }

    #[test]
    fn modified_valid_copy_loads_with_a_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let mut files = embedded();
        files[3].1.push_str("\nPackager note.\n");
        write_bundle(tmp.path(), &files);
        let resolved = resolve_awareness_plugin_at(tmp.path());
        assert_eq!(resolved.source, PluginSource::Installed);
        assert!(resolved.files[3].1.contains("Packager note."));
        assert!(resolved.warnings[0].contains("differs from the compiled copy"));
    }

    #[test]
    fn missing_or_malformed_copy_falls_back_to_embedded() {
        let missing = tempfile::tempdir().unwrap();
        let resolved = resolve_awareness_plugin_at(missing.path());
        assert_eq!(resolved.source, PluginSource::Embedded);
        assert!(resolved.warnings[0].contains("missing or unreadable"));

        let malformed = tempfile::tempdir().unwrap();
        let mut files = embedded();
        files[2].1 = "{}".into();
        write_bundle(malformed.path(), &files);
        let resolved = resolve_awareness_plugin_at(malformed.path());
        assert_eq!(resolved.source, PluginSource::Embedded);
        assert!(resolved.warnings[0].contains("malformed"), "{:?}", resolved.warnings);
        assert_eq!(resolved.files[2].1, EMBEDDED_FILES[2].1);
    }
}
