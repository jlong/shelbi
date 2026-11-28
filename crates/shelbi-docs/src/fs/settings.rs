// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-fs/src/settings.rs), MIT in Shelbi.

//! Settings YAML schemas for spaces and projects (folders).
//!
//! Shelbi drops ContextStore's global MCP config (`.mcp.yml`), and keeps
//! every key of a space's `settings.yml` it doesn't model in
//! [`SpaceSettings::extra`] so a rewrite never loses them.
//!
//! Currently exposes the on-disk schemas plus [`load_space_settings`],
//! which implements the corruption-recovery behavior the Swift app
//! depends on: if the file fails to decode as YAML, split on `\n\n`
//! and decode the last document, then rewrite the file clean.
//!
//! Atomic-write integration on the save side comes with the
//! "Atomic write tested under mid-write failure" DoD box.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fs::paths::atomic_write;
use crate::fs::FsError;

// ─── Space (committed) ──────────────────────────────────────────────

/// Settings stored in `<space>/.contextstore/settings.yml`. Committed to git.
///
/// On-disk YAML uses camelCase keys (the Swift writer's convention):
/// `createdAt`, `projectOrder`, `folderOrder`, `showFrontmatter`, etc.
/// Rust field names stay snake_case; serde bridges the two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SpaceSettings {
    /// Human-readable space name shown in the UI.
    pub name: String,

    /// ISO 8601 timestamp recorded when the space was first created.
    pub created_at: String,

    /// Legacy `github:` block, decoded for migration but **never** re-encoded.
    /// The connection lives outside the space-settings file (in ContextStore,
    /// its app; in Shelbi, the project's git remote), so settings.yml stops
    /// mentioning it once rewritten.
    #[serde(skip_serializing)]
    pub github: Option<LegacyGitHubRepoInfo>,

    /// Legacy `repository:` block, predates [`Self::github`]. Decoded for
    /// migration but **never** re-encoded.
    #[serde(skip_serializing)]
    pub repository: Option<LegacyUrlRepoInfo>,

    /// Ordered list of project names for the sidebar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_order: Option<Vec<String>>,

    /// Per-project ordering of folders shown in the sidebar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_order: Option<BTreeMap<String, Vec<String>>>,

    /// Whether to render YAML frontmatter in document previews.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_frontmatter: Option<bool>,

    /// Whether this space participates in MCP indexing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_enabled: Option<bool>,

    /// Naming convention applied to new document filenames.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name_style: Option<FileNameStyle>,

    /// When true, renaming a document's first heading also renames the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_file_name_with_heading: Option<bool>,

    /// Patterns to rename rather than fully ignoring when matched by
    /// `.csignore` rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rename_ignored_files: Option<Vec<String>>,

    /// Path from the repository root to the space's content directory
    /// (used by orphan-branch deployments).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,

    /// Default branch name to use when creating a fresh remote.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,

    /// Every other top-level key, in file order, written back unchanged.
    ///
    /// Shelbi addition: ContextStore (or a newer version of it) may write
    /// keys this struct doesn't model, and docs and a ContextStore space
    /// are the same folder, so a load-modify-save here must not drop them.
    #[serde(flatten, skip_serializing_if = "yaml_serde::Mapping::is_empty")]
    pub extra: yaml_serde::Mapping,
}

/// Filename casing convention applied when creating or renaming documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum FileNameStyle {
    /// `kebab-case-names.md` (default).
    #[default]
    KebabCase,

    /// `camelCaseNames.md`.
    CamelCase,

    /// Preserve the original casing the user typed.
    Original,
}

/// Legacy GitHub repository info — owner + repo + optional default branch.
///
/// Decoded for backwards compatibility; never re-encoded once the space
/// has been migrated to the newer connection representation living
/// elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyGitHubRepoInfo {
    /// Repository owner (user or org).
    pub owner: String,

    /// Repository name.
    pub repo: String,

    /// Default branch reported by GitHub at the time of connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
}

/// Legacy URL-based repository info — a single git URL with no extra metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyUrlRepoInfo {
    /// Clone URL (HTTPS or SSH).
    pub url: String,
}

// ─── Space (local, gitignored) ──────────────────────────────────────

/// Settings stored in `<space>/.contextstore/settings.local.yml`. Gitignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocalSpaceSettings {
    /// How the space syncs with its remote (auto-sync vs. manual push/pull).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_mode: Option<SyncMode>,

    /// Branches this machine created through the app's branch switcher. A
    /// local hint only: they count as ContextStore branches without probing
    /// their trees, so a new branch is listed the moment it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_branches: Option<Vec<String>>,
}

/// Synchronization mode for a space's remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncMode {
    /// Auto-fetch and auto-push on every relevant local change.
    Auto,

    /// Wait for explicit `cstore push` / `cstore pull` from the user.
    Manual,
}

// ─── Project ────────────────────────────────────────────────────────

/// Settings stored in `<space>/<project>/.contextstore/settings.yml`,
/// unwrapped from the on-disk [`ProjectSettingsFile`] envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSettings {
    /// Display name of the project.
    pub name: String,

    /// Emoji icon shown next to the project in the sidebar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,

    /// Short user-supplied description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// On-disk envelope for a project's `settings.yml`.
///
/// The Swift writer wraps the actual fields under a `project:` key
/// with a top-level `version:` field:
///
/// ```yaml
/// version: 1
/// project:
///   name: "Product"
///   icon: "📦"
///   description: "Product vision, roadmap, and user stories"
/// ```
///
/// Reads / writes always go through this struct so the on-disk shape
/// stays Swift-compatible; the API surface ([`ProjectSettings`]) is
/// the unwrapped inner value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSettingsFile {
    /// Schema version. Swift currently writes `1`; absent on legacy
    /// hand-written files. Decoded for forward-compat and round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,

    /// Unwrapped project metadata.
    pub project: ProjectSettings,
}

impl ProjectSettingsFile {
    /// Schema version emitted on every fresh write.
    pub const CURRENT_VERSION: u32 = 1;

    /// Wrap `settings` in a fresh envelope at [`Self::CURRENT_VERSION`].
    pub fn current(settings: ProjectSettings) -> Self {
        Self {
            version: Some(Self::CURRENT_VERSION),
            project: settings,
        }
    }
}

/// On-disk form of a project's `settings.yml`, accepting both the
/// current wrapped envelope ([`ProjectSettingsFile`]) and the legacy
/// flat form some Swift versions wrote (just the [`ProjectSettings`]
/// fields at the top level — no `version:` line, no `project:` key).
///
/// New writes always use the wrapped form; reads tolerate either so
/// existing spaces created by older Swift builds still parse.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ProjectSettingsOnDisk {
    /// Canonical form: `{ version: u32, project: ProjectSettings }`.
    Wrapped(ProjectSettingsFile),
    /// Legacy flat form: bare [`ProjectSettings`] at the top level.
    Flat(ProjectSettings),
}

impl ProjectSettingsOnDisk {
    /// Unwrap into the API-surface [`ProjectSettings`].
    pub fn into_settings(self) -> ProjectSettings {
        match self {
            Self::Wrapped(f) => f.project,
            Self::Flat(s) => s,
        }
    }
}

// ─── Load (with corruption recovery) ────────────────────────────────

/// Returns the path of `<space_root>/.contextstore/settings.yml`.
pub fn space_settings_path(space_root: &Path) -> PathBuf {
    space_root.join(".contextstore").join("settings.yml")
}

/// Load the space's `settings.yml`, recovering from common corruption.
///
/// Behavior:
///
/// 1. Read the file. Missing files return [`FsError::Io`] with
///    [`std::io::ErrorKind::NotFound`].
/// 2. Try to decode the full content as a single YAML document.
/// 3. If that fails, split on `\n\n` and try to decode the last segment
///    — matches the Swift app's corruption-recovery behavior for files
///    that have been appended to or partially overwritten by a crashed
///    writer.
/// 4. On a successful recovery, rewrite the file clean so subsequent
///    loads take the fast path. Rewrite failures are returned, since
///    a successfully-decoded-but-not-persisted recovery silently leaves
///    the file in its broken state.
pub fn load_space_settings(space_root: &Path) -> Result<SpaceSettings, FsError> {
    let path = space_settings_path(space_root);
    let raw = std::fs::read_to_string(&path).map_err(|source| FsError::Io {
        path: path.to_string_lossy().into_owned(),
        source,
    })?;

    if let Ok(settings) = yaml_serde::from_str::<SpaceSettings>(&raw) {
        return Ok(settings);
    }

    let last_segment = raw.rsplit("\n\n").next().unwrap_or("").trim_start();
    let recovered = yaml_serde::from_str::<SpaceSettings>(last_segment).map_err(|source| {
        FsError::SettingsParse {
            path: path.to_string_lossy().into_owned(),
            source,
        }
    })?;

    let clean = yaml_serde::to_string(&recovered).map_err(FsError::SettingsEncode)?;
    atomic_write(&path, clean.as_bytes())?;

    Ok(recovered)
}

// ─── Save ───────────────────────────────────────────────────────────

/// Write `settings` to `<space_root>/.contextstore/settings.yml`.
///
/// Creates `.contextstore/` if missing.
pub fn save_space_settings(space_root: &Path, settings: &SpaceSettings) -> Result<(), FsError> {
    let yaml = yaml_serde::to_string(settings).map_err(FsError::SettingsEncode)?;
    atomic_write(&space_settings_path(space_root), yaml.as_bytes())
}

// ─── Local space settings (gitignored) ──────────────────────────────

/// Returns `<space_root>/.contextstore/settings.local.yml`.
pub fn local_space_settings_path(space_root: &Path) -> PathBuf {
    space_root.join(".contextstore").join("settings.local.yml")
}

/// Load `settings.local.yml`. Returns [`LocalSpaceSettings::default`]
/// when missing or undecodable.
pub fn load_local_space_settings(space_root: &Path) -> LocalSpaceSettings {
    let path = local_space_settings_path(space_root);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return LocalSpaceSettings::default();
    };
    yaml_serde::from_str::<LocalSpaceSettings>(&raw).unwrap_or_default()
}

/// Write `settings` to `settings.local.yml`. Creates `.contextstore/`
/// if missing.
pub fn save_local_space_settings(
    space_root: &Path,
    settings: &LocalSpaceSettings,
) -> Result<(), FsError> {
    let yaml = yaml_serde::to_string(settings).map_err(FsError::SettingsEncode)?;
    atomic_write(&local_space_settings_path(space_root), yaml.as_bytes())
}

/// Ensure `<space_root>/.gitignore` contains the standard entries
/// (`.DS_Store`, `.contextstore/settings.local.yml`). Append missing
/// entries to an existing file; create the file otherwise.
pub fn ensure_gitignore(space_root: &Path) -> Result<(), FsError> {
    let gi_path = space_root.join(".gitignore");
    const REQUIRED: &[&str] = &[".DS_Store", ".contextstore/settings.local.yml"];

    let existing = std::fs::read_to_string(&gi_path).ok();
    if let Some(content) = existing {
        let mut updated = content.clone();
        let mut changed = false;
        for entry in REQUIRED {
            if !updated.lines().any(|l| l.trim() == *entry) {
                if !updated.ends_with('\n') {
                    updated.push('\n');
                }
                updated.push_str(entry);
                updated.push('\n');
                changed = true;
            }
        }
        if changed {
            atomic_write(&gi_path, updated.as_bytes())?;
        }
    } else {
        let mut content = String::new();
        for entry in REQUIRED {
            content.push_str(entry);
            content.push('\n');
        }
        atomic_write(&gi_path, content.as_bytes())?;
    }
    Ok(())
}

/// Initialize an existing folder `root` as a ContextStore space named
/// `name`.
///
/// Creates `.contextstore/settings.yml`,
/// `.contextstore/templates/blank.md`, `.csignore` (under
/// `root_subdir` when set, otherwise at the root), and `.gitignore`.
///
/// If `root_subdir` is `Some("docs")`, the layout is:
/// ```text
/// <root>/.contextstore/settings.yml   # root: docs, defaultBranch: ...
/// <root>/.contextstore/templates/blank.md
/// <root>/.gitignore
/// <root>/docs/.csignore
/// ```
///
/// Returns the existing settings on disk if `settings.yml` already
/// exists, leaving the file untouched (idempotent).
pub fn initialize_as_space(
    root: &Path,
    name: &str,
    root_subdir: Option<&str>,
    default_branch: Option<String>,
) -> Result<SpaceSettings, FsError> {
    use crate::fs::paths::iso8601_now;

    if space_settings_path(root).is_file() {
        let existing = load_space_settings(root)?;
        ensure_gitignore(root)?;
        return Ok(existing);
    }

    let cs_dir = root.join(".contextstore");
    std::fs::create_dir_all(&cs_dir).map_err(|source| FsError::Io {
        path: cs_dir.to_string_lossy().into_owned(),
        source,
    })?;
    let templates = cs_dir.join("templates");
    std::fs::create_dir_all(&templates).map_err(|source| FsError::Io {
        path: templates.to_string_lossy().into_owned(),
        source,
    })?;
    atomic_write(
        &templates.join("blank.md"),
        crate::fs::defaults::BLANK_TEMPLATE.as_bytes(),
    )?;

    let settings = SpaceSettings {
        name: name.to_string(),
        created_at: iso8601_now(),
        root: root_subdir.map(str::to_string),
        default_branch,
        ..Default::default()
    };
    save_space_settings(root, &settings)?;

    let content_dir = root_subdir
        .map(|s| root.join(s))
        .unwrap_or_else(|| root.to_path_buf());
    if !content_dir.is_dir() {
        std::fs::create_dir_all(&content_dir).map_err(|source| FsError::Io {
            path: content_dir.to_string_lossy().into_owned(),
            source,
        })?;
    }
    let csignore = content_dir.join(".csignore");
    if !csignore.exists() {
        atomic_write(&csignore, crate::fs::defaults::CSIGNORE.as_bytes())?;
    }
    ensure_gitignore(root)?;

    Ok(settings)
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_roundtrip_space_settings_minimal() {
        let original = SpaceSettings {
            name: "Planning".to_string(),
            created_at: "2026-05-01T12:00:00Z".to_string(),
            ..Default::default()
        };
        let yaml = yaml_serde::to_string(&original).expect("encode");
        let restored: SpaceSettings = yaml_serde::from_str(&yaml).expect("decode");
        assert_eq!(original, restored);
    }

    #[test]
    fn yaml_roundtrip_space_settings_full() {
        let mut folder_order = BTreeMap::new();
        folder_order.insert(
            "Engineering".to_string(),
            vec!["Architecture".to_string(), "ADRs".to_string()],
        );
        let original = SpaceSettings {
            name: "Planning".to_string(),
            created_at: "2026-05-01T12:00:00Z".to_string(),
            github: None,
            repository: None,
            project_order: Some(vec!["Engineering".into(), "Product".into()]),
            folder_order: Some(folder_order),
            show_frontmatter: Some(false),
            mcp_enabled: Some(true),
            file_name_style: Some(FileNameStyle::KebabCase),
            sync_file_name_with_heading: Some(true),
            rename_ignored_files: Some(vec!["*.draft.md".into()]),
            root: Some("content".into()),
            default_branch: Some("main".into()),
            extra: yaml_serde::Mapping::new(),
        };
        let yaml = yaml_serde::to_string(&original).expect("encode");
        let restored: SpaceSettings = yaml_serde::from_str(&yaml).expect("decode");
        assert_eq!(original, restored);
    }

    #[test]
    fn legacy_github_decodes_from_yaml() {
        let yaml = "name: Planning\n\
                    createdAt: '2026-05-01T12:00:00Z'\n\
                    github:\n  \
                      owner: jlong\n  \
                      repo: planning\n  \
                      defaultBranch: main\n";
        let decoded: SpaceSettings = yaml_serde::from_str(yaml).expect("decode");
        assert_eq!(
            decoded.github,
            Some(LegacyGitHubRepoInfo {
                owner: "jlong".into(),
                repo: "planning".into(),
                default_branch: Some("main".into()),
            })
        );
    }

    #[test]
    fn legacy_github_is_dropped_on_reencode() {
        let with_legacy = SpaceSettings {
            name: "Planning".into(),
            created_at: "2026-05-01T12:00:00Z".into(),
            github: Some(LegacyGitHubRepoInfo {
                owner: "jlong".into(),
                repo: "planning".into(),
                default_branch: Some("main".into()),
            }),
            ..Default::default()
        };
        let yaml = yaml_serde::to_string(&with_legacy).expect("encode");
        assert!(
            !yaml.contains("github"),
            "legacy `github` must not be re-encoded; got:\n{yaml}"
        );
        let restored: SpaceSettings = yaml_serde::from_str(&yaml).expect("decode");
        assert!(restored.github.is_none());
    }

    #[test]
    fn legacy_repository_decodes_from_yaml() {
        let yaml = "name: Planning\n\
                    createdAt: '2026-05-01T12:00:00Z'\n\
                    repository:\n  \
                      url: git@github.com:jlong/planning.git\n";
        let decoded: SpaceSettings = yaml_serde::from_str(yaml).expect("decode");
        assert_eq!(
            decoded.repository,
            Some(LegacyUrlRepoInfo {
                url: "git@github.com:jlong/planning.git".into(),
            })
        );
    }

    #[test]
    fn legacy_repository_is_dropped_on_reencode() {
        let with_legacy = SpaceSettings {
            name: "Planning".into(),
            created_at: "2026-05-01T12:00:00Z".into(),
            repository: Some(LegacyUrlRepoInfo {
                url: "git@github.com:jlong/planning.git".into(),
            }),
            ..Default::default()
        };
        let yaml = yaml_serde::to_string(&with_legacy).expect("encode");
        assert!(
            !yaml.contains("repository"),
            "legacy `repository` must not be re-encoded; got:\n{yaml}"
        );
        let restored: SpaceSettings = yaml_serde::from_str(&yaml).expect("decode");
        assert!(restored.repository.is_none());
    }

    #[test]
    fn unknown_fields_decode_without_error() {
        // YAML produced by a future version of the app may contain fields
        // we don't know about yet — decoding must succeed (forward-compat
        // per the sub-plan's risks section).
        let yaml = "name: Planning\n\
                    createdAt: '2026-05-01T12:00:00Z'\n\
                    futureField: hello\n\
                    anotherFutureField:\n  \
                      nested: value\n";
        let decoded: SpaceSettings = yaml_serde::from_str(yaml).expect("decode");
        assert_eq!(decoded.name, "Planning");
    }

    #[test]
    fn yaml_roundtrip_local_space_settings() {
        let original = LocalSpaceSettings {
            sync_mode: Some(SyncMode::Auto),
            managed_branches: Some(vec!["draft/q3-notes".to_string()]),
        };
        let yaml = yaml_serde::to_string(&original).expect("encode");
        let restored: LocalSpaceSettings = yaml_serde::from_str(&yaml).expect("decode");
        assert_eq!(original, restored);
    }

    #[test]
    fn yaml_roundtrip_project_settings() {
        let original = ProjectSettings {
            name: "Engineering".into(),
            icon: Some("⚙️".into()),
            description: Some("Implementation docs and ADRs.".into()),
        };
        let yaml = yaml_serde::to_string(&original).expect("encode");
        let restored: ProjectSettings = yaml_serde::from_str(&yaml).expect("decode");
        assert_eq!(original, restored);
    }

    #[test]
    fn project_settings_file_decodes_swift_app_yaml() {
        // Byte-for-byte shape of what the Swift app writes to
        // <space>/<project>/.contextstore/settings.yml.
        let yaml = "version: 1\n\
                    project:\n  \
                      name: \"Product\"\n  \
                      icon: \"📦\"\n  \
                      description: \"Product vision, roadmap, and user stories\"\n";
        let parsed: ProjectSettingsFile = yaml_serde::from_str(yaml).expect("decode");
        assert_eq!(parsed.version, Some(1));
        assert_eq!(parsed.project.name, "Product");
        assert_eq!(parsed.project.icon.as_deref(), Some("📦"));
        assert_eq!(
            parsed.project.description.as_deref(),
            Some("Product vision, roadmap, and user stories")
        );
    }

    #[test]
    fn project_settings_on_disk_accepts_wrapped_form() {
        let yaml = "version: 1\nproject:\n  name: Product\n  icon: \"📦\"\n";
        let parsed: ProjectSettingsOnDisk = yaml_serde::from_str(yaml).expect("decode wrapped");
        let settings = parsed.into_settings();
        assert_eq!(settings.name, "Product");
        assert_eq!(settings.icon.as_deref(), Some("📦"));
    }

    #[test]
    fn project_settings_on_disk_accepts_legacy_flat_form() {
        // Some older Swift builds wrote settings.yml without the
        // version/project envelope — just the bare ProjectSettings
        // fields at the top level. Real ContextStore-docs has these.
        let yaml = "name: Personas\n";
        let parsed: ProjectSettingsOnDisk = yaml_serde::from_str(yaml).expect("decode legacy flat");
        let settings = parsed.into_settings();
        assert_eq!(settings.name, "Personas");
        assert!(settings.icon.is_none());
        assert!(settings.description.is_none());
    }

    #[test]
    fn project_settings_file_round_trips_with_current_version() {
        let inner = ProjectSettings {
            name: "Product".into(),
            icon: Some("📦".into()),
            description: Some("Vision".into()),
        };
        let original = ProjectSettingsFile::current(inner.clone());
        let yaml = yaml_serde::to_string(&original).expect("encode");
        // Top-level keys are `version` + `project` — match Swift's shape.
        assert!(yaml.starts_with("version: 1\n"), "got:\n{yaml}");
        assert!(yaml.contains("\nproject:\n"), "got:\n{yaml}");
        let restored: ProjectSettingsFile = yaml_serde::from_str(&yaml).expect("decode");
        assert_eq!(restored.version, Some(ProjectSettingsFile::CURRENT_VERSION));
        assert_eq!(restored.project, inner);
    }

    #[test]
    fn file_name_style_default_is_kebab_case() {
        assert_eq!(FileNameStyle::default(), FileNameStyle::KebabCase);
    }

    // ─── Corruption recovery ────────────────────────────────────────

    fn write_settings_file(space_root: &Path, contents: &str) {
        let dir = space_root.join(".contextstore");
        std::fs::create_dir_all(&dir).expect("mkdir .contextstore");
        std::fs::write(dir.join("settings.yml"), contents).expect("write settings.yml");
    }

    #[test]
    fn load_space_settings_decodes_clean_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_settings_file(
            tmp.path(),
            "name: Planning\ncreatedAt: '2026-05-01T12:00:00Z'\n",
        );

        let loaded = load_space_settings(tmp.path()).expect("load");
        assert_eq!(loaded.name, "Planning");
        assert_eq!(loaded.created_at, "2026-05-01T12:00:00Z");
    }

    #[test]
    fn load_space_settings_missing_file_returns_not_found_io_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = load_space_settings(tmp.path()).expect_err("expected NotFound");
        match err {
            FsError::Io { source, .. } => {
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected FsError::Io, got {other:?}"),
        }
    }

    #[test]
    fn load_space_settings_recovers_from_corruption_and_rewrites_clean() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Simulate the failure mode the Swift app handles: stale/garbage
        // YAML at the top of the file followed by a `\n\n` delimiter and
        // the actual current document. Real-world causes include a
        // crashed writer that produced an interleaved partial encode.
        let corrupted = "garbage_that_isnt_a_struct: [unterminated\n\
                         \n\
                         name: Planning\n\
                         createdAt: '2026-05-01T12:00:00Z'\n";
        write_settings_file(tmp.path(), corrupted);

        let loaded = load_space_settings(tmp.path()).expect("recover");
        assert_eq!(loaded.name, "Planning");
        assert_eq!(loaded.created_at, "2026-05-01T12:00:00Z");

        // After recovery, the file on disk must be the clean version —
        // a subsequent load takes the fast path (no recovery needed).
        let rewritten = std::fs::read_to_string(space_settings_path(tmp.path()))
            .expect("read rewritten settings.yml");
        assert!(!rewritten.contains("garbage_that_isnt_a_struct"));
        let reloaded: SpaceSettings = yaml_serde::from_str(&rewritten).expect("decode clean");
        assert_eq!(reloaded, loaded);
    }

    #[test]
    fn load_space_settings_unrecoverable_returns_parse_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // No `\n\n` separator and no valid last document — corruption
        // recovery has nothing to fall back to.
        write_settings_file(tmp.path(), "definitely_not: [valid yaml");

        let err = load_space_settings(tmp.path()).expect_err("expected parse error");
        assert!(matches!(err, FsError::SettingsParse { .. }));
    }
}
