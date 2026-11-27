// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-fs/src/space.rs), MIT in Shelbi.

//! Space registry, discovery, and (eventually) CRUD.
//!
//! ContextStore knows about a space via three pathways, per the sub-plan:
//!
//!   1. The space registry at `<base_dir>/.contextstore/spaces.yml`
//!      (YAML list of [`RegisteredSpace`] entries the user has
//!      explicitly tracked).
//!   2. A scan of the default base directory
//!      (e.g. `~/Documents/ContextStore`).
//!   3. macOS security-scoped bookmarks resolved by the Swift app and
//!      handed to Rust as absolute paths — from the core's point of
//!      view, these arrive via [`SpaceManager::track`] and end up in
//!      the same registry as path (1).
//!
//! [`SpaceManager::list`] merges (1) and (2), deduplicating spaces that
//! happen to live under the base directory *and* have been registered
//! explicitly. Spaces are only surfaced when their
//! `.contextstore/settings.yml` is present and decodes.

use std::path::{Path, PathBuf};

use crate::domain::Space;
use serde::{Deserialize, Serialize};

use crate::fs::paths::atomic_write;
use crate::fs::settings::{load_space_settings, space_settings_path};
use crate::fs::FsError;

/// On-disk registry entry: one tracked space.
///
/// Serialized as a YAML mapping inside the top-level sequence in
/// `<base_dir>/.contextstore/spaces.yml`. Optional fields are omitted
/// when unset rather than written as `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredSpace {
    /// Absolute, canonical path to the space root.
    pub path: PathBuf,
    /// Clone URL when added via `cstore clone`; `git remote get-url
    /// origin` when added via `cstore add`; otherwise omitted.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub origin_url: Option<String>,
    /// RFC 3339 UTC timestamp of registry insertion. Never mutated.
    pub added_at: String,
    /// RFC 3339 UTC timestamp of the last open. Updated by the app and
    /// by `cstore open <path>`; not by other CLI reads.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_opened_at: Option<String>,
}

/// The registry file's contents: the spaces ContextStore knows about, and the
/// ones it has been told to forget.
///
/// `ignored` exists because the base-directory scan is unconditional. Removing a
/// space that lives *inside* `base_dir` dropped it from the registry and the
/// very next scan put it straight back, so "Remove Space" did nothing at all for
/// exactly the spaces most users have. An ignored path is a tombstone: the files
/// stay on disk, the scan keeps finding them, and the app keeps not showing
/// them until the user adds the space back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SpacesFile {
    /// Spaces the user has explicitly tracked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spaces: Vec<RegisteredSpace>,
    /// Directories under `base_dir` that hold a space the user removed. Absolute
    /// paths, so a renamed directory is simply discovered afresh.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored: Vec<PathBuf>,
}

impl<'de> Deserialize<'de> for SpacesFile {
    /// Accepts both shapes: the current mapping, and the bare sequence of
    /// entries this file used to be. An installation that predates `ignored`
    /// reads back as spaces with nothing ignored, which is what it meant.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape {
            Modern {
                #[serde(default)]
                spaces: Vec<RegisteredSpace>,
                #[serde(default)]
                ignored: Vec<PathBuf>,
            },
            Legacy(Vec<RegisteredSpace>),
        }
        Ok(match Shape::deserialize(deserializer)? {
            Shape::Modern { spaces, ignored } => SpacesFile { spaces, ignored },
            Shape::Legacy(spaces) => SpacesFile {
                spaces,
                ignored: Vec::new(),
            },
        })
    }
}

/// The registry file's path inside a base directory.
///
/// It lives with the spaces it describes rather than in a per-machine Library
/// directory, so moving or syncing the folder carries the list along.
pub fn registry_path_in(base_dir: &Path) -> PathBuf {
    base_dir.join(".contextstore").join("spaces.yml")
}

/// Discovers, registers, and (later) creates ContextStore spaces.
pub struct SpaceManager {
    base_dir: PathBuf,
    registry_path: PathBuf,
}

impl SpaceManager {
    /// Construct a manager with an explicit base directory and an
    /// explicit registry file path. Tests use this directly to point
    /// `spaces.yml` at a tempdir; the CLI builds it from
    /// `--base-dir` / `--registry` (and their env equivalents).
    pub fn with_registry_path(base_dir: PathBuf, registry_path: PathBuf) -> Self {
        Self {
            base_dir,
            registry_path,
        }
    }

    /// Legacy constructor: `<config_dir>/spaces.yml` is used as the
    /// registry file. Kept so the existing cstore-core / cstore-cli /
    /// cstore-mcp callers compile while the `--base-dir` /
    /// `--registry` plumbing lands.
    pub fn with_dirs(base_dir: PathBuf, config_dir: PathBuf) -> Self {
        let registry_path = config_dir.join("spaces.yml");
        Self::with_registry_path(base_dir, registry_path)
    }

    /// The base directory this manager scans for spaces (read-only here).
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// The registry file this manager reads and writes.
    pub fn registry_path(&self) -> &Path {
        &self.registry_path
    }

    /// The whole registry file. A missing, empty or unparseable file reads as
    /// "nothing tracked, nothing ignored" — the registry is a cache of the
    /// user's choices, not the source of truth for what exists on disk.
    pub fn load_file(&self) -> SpacesFile {
        match std::fs::read(&self.registry_path) {
            Ok(bytes) if bytes.is_empty() => SpacesFile::default(),
            Ok(bytes) => yaml_serde::from_slice::<SpacesFile>(&bytes).unwrap_or_default(),
            Err(_) => SpacesFile::default(),
        }
    }

    fn load_registry(&self) -> Vec<RegisteredSpace> {
        self.load_file().spaces
    }

    fn save_file(&self, file: &SpacesFile) -> Result<(), FsError> {
        let yaml = if file.spaces.is_empty() && file.ignored.is_empty() {
            // Round-trip stability: an empty registry is an empty file.
            // `yaml_serde` would otherwise emit `{}`, which reads back the same
            // but is a worse on-disk artifact.
            String::new()
        } else {
            yaml_serde::to_string(file).map_err(FsError::SettingsEncode)?
        };
        atomic_write(&self.registry_path, yaml.as_bytes())
    }

    fn save_registry(&self, entries: &[RegisteredSpace]) -> Result<(), FsError> {
        let mut file = self.load_file();
        file.spaces = entries.to_vec();
        self.save_file(&file)
    }

    /// Replace the entire registry with `entries`. Used by the
    /// Swift → YAML migration and by tests that want to seed a
    /// specific on-disk shape.
    pub fn write_registry(&self, entries: Vec<RegisteredSpace>) -> Result<(), FsError> {
        self.save_registry(&entries)
    }

    /// Return all registered spaces verbatim. Reads the registry from
    /// disk every call; the manager does not cache.
    pub fn list_registered(&self) -> Vec<RegisteredSpace> {
        self.load_registry()
    }

    /// Insert `entry` into the registry. Errors if the path is already
    /// registered.
    pub fn add_registered_path(&self, entry: RegisteredSpace) -> Result<(), FsError> {
        let mut entries = self.load_registry();
        if entries.iter().any(|e| e.path == entry.path) {
            return Err(FsError::Io {
                path: entry.path.to_string_lossy().into_owned(),
                source: std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "path is already registered",
                ),
            });
        }
        // Adding a space back is how a removal is undone, so the tombstone goes
        // with it — otherwise the entry would be listed and ignored at once.
        let mut file = self.load_file();
        file.ignored.retain(|p| p != &entry.path);
        entries.push(entry);
        file.spaces = entries;
        self.save_file(&file)
    }

    /// Stop showing the space at `root`, leaving its files alone.
    ///
    /// Removes it from the tracked list and, when it lives under `base_dir`,
    /// records a tombstone so the unconditional scan does not immediately put it
    /// back. A space outside `base_dir` needs no tombstone: nothing rediscovers
    /// it, and writing one would grow the file forever as external spaces come
    /// and go.
    pub fn ignore(&self, root: &Path) -> Result<(), FsError> {
        let mut file = self.load_file();
        file.spaces.retain(|e| e.path != root);
        if root.starts_with(&self.base_dir) && !file.ignored.iter().any(|p| p == root) {
            file.ignored.push(root.to_path_buf());
        }
        self.save_file(&file)
    }

    /// Forget a tombstone, so the scan surfaces `root` again.
    pub fn unignore(&self, root: &Path) -> Result<(), FsError> {
        let mut file = self.load_file();
        let before = file.ignored.len();
        file.ignored.retain(|p| p != root);
        if file.ignored.len() == before {
            return Ok(());
        }
        self.save_file(&file)
    }

    /// The paths currently tombstoned.
    pub fn ignored(&self) -> Vec<PathBuf> {
        self.load_file().ignored
    }

    /// True when `path` is in the registry. Comparison is on the
    /// exact `PathBuf` value; canonicalization is the caller's
    /// responsibility (so a stale registry entry whose target has been
    /// deleted is still "registered" until it is removed).
    pub fn is_registered(&self, path: &Path) -> Result<bool, FsError> {
        Ok(self.load_registry().iter().any(|e| e.path == path))
    }

    /// Record that a registered space was just opened. Updates
    /// `last_opened_at` and writes the registry. No-op when `path` is
    /// not registered.
    pub fn touch_last_opened(&self, path: &Path) -> Result<(), FsError> {
        let mut entries = self.load_registry();
        let mut changed = false;
        for entry in entries.iter_mut() {
            if entry.path == path {
                entry.last_opened_at = Some(crate::fs::paths::iso8601_now());
                changed = true;
                break;
            }
        }
        if changed {
            self.save_registry(&entries)?;
        }
        Ok(())
    }

    /// Add `root` to the registry. Idempotent: registering the same path
    /// twice leaves a single entry. Path-only convenience wrapper around
    /// [`Self::add_registered_path`] for callers (notably the existing
    /// `cstore new` flow) that don't yet plumb `RegisteredSpace`
    /// through.
    pub fn track(&self, root: &Path) -> Result<(), FsError> {
        if self.is_registered(root)? {
            return Ok(());
        }
        let entry = RegisteredSpace {
            path: root.to_path_buf(),
            origin_url: None,
            added_at: crate::fs::paths::iso8601_now(),
            last_opened_at: None,
        };
        self.add_registered_path(entry)
    }

    /// All known spaces: the registry merged with a base-directory scan,
    /// deduplicated by absolute path.
    pub fn list(&self) -> Result<Vec<Space>, FsError> {
        let file = self.load_file();
        let ignored = file.ignored;
        // An explicitly tracked space outranks the ignore list: adding one back
        // is how the user undoes a removal, and `add_registered_path` clears the
        // tombstone, so anything still listed here is meant to be shown.
        let mut roots: Vec<PathBuf> = file.spaces.into_iter().map(|e| e.path).collect();

        if let Ok(entries) = std::fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if !space_settings_path(&path).is_file() {
                    continue;
                }
                // A space the user removed is still on disk, and the scan will
                // keep finding it. Without this the removal was undone before
                // the user saw it happen.
                if ignored.iter().any(|i| i == &path) {
                    continue;
                }
                if !roots.iter().any(|existing| existing == &path) {
                    roots.push(path);
                }
            }
        }

        let mut spaces = Vec::with_capacity(roots.len());
        for root in roots {
            if !space_settings_path(&root).is_file() {
                // Skip stale registry entries pointing at directories
                // whose settings.yml no longer exists; the user may have
                // moved or deleted the space outside the app.
                continue;
            }
            // An unreadable space is skipped for the same reason a missing one
            // is, rather than failing the whole call. Under the App Sandbox a
            // space outside the container can be stat'd but not opened once its
            // security-scoped bookmark is gone — and propagating that error here
            // returned *no* spaces at all, so one lapsed grant hid every other
            // space the user had, including the ones inside the container that
            // never needed a grant. The app then showed its first-run screen as
            // though nothing had ever been set up.
            let Ok(settings) = load_space_settings(&root) else {
                continue;
            };
            spaces.push(Space {
                name: settings.name,
                path: root.to_string_lossy().into_owned(),
                created_at: settings.created_at,
            });
        }
        Ok(spaces)
    }

    /// Create a new space under [`Self::base_dir`].
    ///
    /// Slugs `name` into a directory name, then delegates to
    /// [`Self::init_at_path`] for the actual on-disk layout +
    /// registration. The CLI's legacy `cstore new --space "<name>"`
    /// form keeps using this path; the new `cstore init <path>` verb
    /// goes through `init_at_path` directly.
    pub fn create(&self, name: &str) -> Result<Space, FsError> {
        use crate::fs::paths::kebab_slug;

        let slug = kebab_slug(name);
        if slug.is_empty() {
            return Err(FsError::Io {
                path: self.base_dir.to_string_lossy().into_owned(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "space name is empty after sanitization",
                ),
            });
        }
        let root = self.base_dir.join(&slug);
        self.init_at_path(&root, name)
    }

    /// Initialize (or recognize) a space at `root` with display name
    /// `name`.
    ///
    /// Idempotent — if `root` already contains a
    /// `.contextstore/settings.yml`, the existing settings are loaded
    /// and the space is just made sure to be in the registry. The
    /// supplied `name` is ignored in that case; the on-disk settings
    /// win.
    ///
    /// Initializes the standard on-disk layout:
    /// * `<root>/.contextstore/settings.yml` (with `name` + `createdAt`)
    /// * `<root>/.contextstore/templates/blank.md`
    /// * `<root>/.csignore` (default ignore patterns)
    /// * `<root>/.gitignore` (`.DS_Store` + local settings)
    ///
    /// Then registers the space with the manager. Does **not** run
    /// `git init`; the CLI layer orchestrates the git bootstrap on
    /// top of this call so non-git contexts (tests, MCP) can opt out.
    pub fn init_at_path(&self, root: &Path, name: &str) -> Result<Space, FsError> {
        use crate::fs::paths::iso8601_now;

        // Idempotent path: settings.yml already exists.
        if crate::fs::settings::space_settings_path(root).is_file() {
            let existing = crate::fs::settings::load_space_settings(root)?;
            self.track(root)?;
            return Ok(Space {
                name: existing.name,
                path: root.to_string_lossy().into_owned(),
                created_at: existing.created_at,
            });
        }

        let templates_dir = root.join(".contextstore").join("templates");
        std::fs::create_dir_all(&templates_dir).map_err(|source| FsError::Io {
            path: templates_dir.to_string_lossy().into_owned(),
            source,
        })?;

        atomic_write(
            &templates_dir.join("blank.md"),
            crate::fs::defaults::BLANK_TEMPLATE.as_bytes(),
        )?;

        let created_at = iso8601_now();
        let settings = crate::fs::settings::SpaceSettings {
            name: name.to_string(),
            created_at: created_at.clone(),
            ..Default::default()
        };
        let yaml = yaml_serde::to_string(&settings).map_err(FsError::SettingsEncode)?;
        atomic_write(&crate::fs::settings::space_settings_path(root), yaml.as_bytes())?;

        atomic_write(
            &root.join(".csignore"),
            crate::fs::defaults::CSIGNORE.as_bytes(),
        )?;
        atomic_write(
            &root.join(".gitignore"),
            crate::fs::defaults::GITIGNORE.as_bytes(),
        )?;

        self.track(root)?;

        Ok(Space {
            name: name.to_string(),
            path: root.to_string_lossy().into_owned(),
            created_at,
        })
    }

    /// Walk up from `start` looking for `.contextstore/settings.yml`.
    /// Returns the space root, or `None` if `start` is not inside a space.
    pub fn find_root(start: &Path) -> Option<PathBuf> {
        let mut current = start;
        loop {
            if space_settings_path(current).is_file() {
                return Some(current.to_path_buf());
            }
            current = current.parent()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_space(root: &Path, name: &str) {
        let dir = root.join(".contextstore");
        std::fs::create_dir_all(&dir).expect("mkdir .contextstore");
        let yaml = format!("name: {name}\ncreatedAt: '2026-05-01T12:00:00Z'\n");
        std::fs::write(dir.join("settings.yml"), yaml).expect("write settings.yml");
    }

    fn make_manager() -> (tempfile::TempDir, tempfile::TempDir, SpaceManager) {
        let base = tempfile::tempdir().expect("base tempdir");
        let config = tempfile::tempdir().expect("config tempdir");
        let manager =
            SpaceManager::with_dirs(base.path().to_path_buf(), config.path().to_path_buf());
        (base, config, manager)
    }

    #[test]
    fn list_is_empty_when_no_spaces_anywhere() {
        let (_b, _c, manager) = make_manager();
        assert!(manager.list().expect("list").is_empty());
    }

    // ─── Removing a space that lives inside base_dir ────────────────

    #[test]
    fn an_ignored_space_inside_base_dir_stops_being_listed() {
        // The bug this exists for: the base-directory scan is unconditional, so
        // dropping a registry entry for a space under `base_dir` was undone by
        // the very next scan and "Remove Space" appeared to do nothing.
        let (base, _c, manager) = make_manager();
        let space_root = base.path().join("Notes");
        write_space(&space_root, "Notes");
        assert_eq!(manager.list().expect("list").len(), 1);

        manager.ignore(&space_root).expect("ignore");

        assert!(
            manager.list().expect("list").is_empty(),
            "a removed space was put straight back by the scan"
        );
        assert!(space_root.is_dir(), "removing a space must not delete it");
    }

    #[test]
    fn adding_an_ignored_space_back_clears_its_tombstone() {
        let (base, _c, manager) = make_manager();
        let space_root = base.path().join("Notes");
        write_space(&space_root, "Notes");
        manager.ignore(&space_root).expect("ignore");

        manager.track(&space_root).expect("track");

        assert_eq!(manager.list().expect("list").len(), 1);
        assert!(
            manager.ignored().is_empty(),
            "a space listed and ignored at once is a contradiction"
        );
    }

    #[test]
    fn unignore_surfaces_the_space_again() {
        let (base, _c, manager) = make_manager();
        let space_root = base.path().join("Notes");
        write_space(&space_root, "Notes");
        manager.ignore(&space_root).expect("ignore");

        manager.unignore(&space_root).expect("unignore");

        assert_eq!(manager.list().expect("list").len(), 1);
    }

    #[test]
    fn a_space_outside_base_dir_needs_no_tombstone() {
        // Nothing rediscovers it, so writing one would grow the file forever as
        // external spaces come and go.
        let (_b, _c, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let space_root = external.path().join("Tracked");
        write_space(&space_root, "Tracked");
        manager.track(&space_root).expect("track");

        manager.ignore(&space_root).expect("ignore");

        assert!(manager.list().expect("list").is_empty());
        assert!(manager.ignored().is_empty());
    }

    #[test]
    fn ignoring_the_same_space_twice_writes_one_tombstone() {
        let (base, _c, manager) = make_manager();
        let space_root = base.path().join("Notes");
        write_space(&space_root, "Notes");

        manager.ignore(&space_root).expect("ignore 1");
        manager.ignore(&space_root).expect("ignore 2");

        assert_eq!(manager.ignored().len(), 1);
    }

    #[test]
    fn a_legacy_registry_file_still_reads() {
        // The file used to be a bare YAML sequence. An installation that
        // predates `ignored` must read back as spaces with nothing ignored,
        // rather than as an empty registry — which would have silently dropped
        // every externally-tracked space on upgrade.
        let (_b, _c, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let space_root = external.path().join("Legacy");
        write_space(&space_root, "Legacy");
        let legacy = format!(
            "- path: {}\n  added_at: '2026-01-01T00:00:00Z'\n",
            space_root.to_string_lossy()
        );
        std::fs::create_dir_all(manager.registry_path().parent().expect("parent"))
            .expect("mkdir");
        std::fs::write(manager.registry_path(), legacy).expect("write legacy registry");

        let file = manager.load_file();
        assert_eq!(file.spaces.len(), 1, "legacy sequence was not read");
        assert!(file.ignored.is_empty());
        assert_eq!(manager.list().expect("list").len(), 1);
    }

    // ─── Discovery path 1: registry ─────────────────────────────────

    #[test]
    fn list_finds_space_in_registry_outside_base_dir() {
        let (_b, _c, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let space_root = external.path().join("Tracked");
        write_space(&space_root, "Tracked");
        manager.track(&space_root).expect("track");

        let spaces = manager.list().expect("list");
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].name, "Tracked");
        assert_eq!(spaces[0].path, space_root.to_string_lossy());
    }

    #[test]
    fn track_is_idempotent() {
        let (_b, _c, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let space_root = external.path().join("Tracked");
        write_space(&space_root, "Tracked");

        manager.track(&space_root).expect("track 1");
        manager.track(&space_root).expect("track 2");
        manager.track(&space_root).expect("track 3");

        let registry = manager.list_registered();
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn list_skips_stale_registry_entries() {
        let (_b, _c, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let space_root = external.path().join("Tracked");
        write_space(&space_root, "Tracked");
        manager.track(&space_root).expect("track");

        // Simulate the user deleting the space outside the app.
        std::fs::remove_dir_all(&space_root).expect("remove space");

        let spaces = manager.list().expect("list");
        assert!(spaces.is_empty());
    }

    // ─── Discovery path 2: base directory scan ──────────────────────

    #[test]
    fn list_finds_space_in_base_dir_without_tracking() {
        let (base, _c, manager) = make_manager();
        let space_root = base.path().join("Planning");
        write_space(&space_root, "Planning");

        let spaces = manager.list().expect("list");
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].name, "Planning");
    }

    #[test]
    fn list_ignores_base_dir_subdirs_without_settings_yml() {
        let (base, _c, manager) = make_manager();
        std::fs::create_dir(base.path().join("not_a_space")).expect("mkdir");

        assert!(manager.list().expect("list").is_empty());
    }

    // ─── Discovery path 3: bookmark-resolved external paths ─────────
    //
    // From the Rust core's perspective, security-scoped bookmarks
    // resolve to absolute paths the Swift side feeds back via `track`,
    // i.e. discovery path (1). This test models that flow: an external
    // space registered explicitly without ever sitting under base_dir.

    #[test]
    fn list_finds_externally_tracked_space_modelling_bookmark_flow() {
        let (_b, _c, manager) = make_manager();

        // Stand-in for a security-scoped bookmark resolution: the Swift
        // app starts accessing a security-scoped resource and hands us
        // the resolved absolute path, which we register.
        let bookmark_target = tempfile::tempdir().expect("bookmark target tempdir");
        let space_root = bookmark_target.path().join("ExternalSpace");
        write_space(&space_root, "ExternalSpace");
        manager.track(&space_root).expect("track resolved bookmark");

        let spaces = manager.list().expect("list");
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].name, "ExternalSpace");
    }

    // ─── Dedup across discovery paths ───────────────────────────────

    #[test]
    fn list_dedups_space_present_in_both_registry_and_base_dir() {
        let (base, _c, manager) = make_manager();
        let space_root = base.path().join("Both");
        write_space(&space_root, "Both");
        manager.track(&space_root).expect("track");

        let spaces = manager.list().expect("list");
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].name, "Both");
    }

    // ─── find_root walk-up ──────────────────────────────────────────

    #[test]
    fn find_root_walks_up_to_settings_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let space_root = tmp.path().join("Space");
        write_space(&space_root, "Space");
        let nested = space_root.join("project").join("a").join("b");
        std::fs::create_dir_all(&nested).expect("mkdir nested");

        let found = SpaceManager::find_root(&nested).expect("walk up");
        assert_eq!(found, space_root);
    }

    #[test]
    fn create_writes_settings_yml_tracks_and_returns_space() {
        let (base, _c, manager) = make_manager();

        let space = manager.create("My Planning Space").expect("create");

        // Folder name is kebab-slugged.
        let expected_root = base.path().join("my-planning-space");
        assert_eq!(space.path, expected_root.to_string_lossy());
        assert_eq!(space.name, "My Planning Space");
        assert!(!space.created_at.is_empty());

        // settings.yml round-trips through load_space_settings.
        let loaded = crate::fs::settings::load_space_settings(&expected_root).expect("load");
        assert_eq!(loaded.name, "My Planning Space");
        assert_eq!(loaded.created_at, space.created_at);

        // It also appears in list (via the registry).
        let listed = manager.list().expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "My Planning Space");
    }

    #[test]
    fn create_writes_seed_files_for_new_space() {
        let (base, _c, manager) = make_manager();
        manager.create("My Space").expect("create");
        let root = base.path().join("my-space");

        // .csignore at the root carries the default ignore patterns.
        let csignore = std::fs::read_to_string(root.join(".csignore")).expect("csignore");
        assert_eq!(csignore, crate::fs::defaults::CSIGNORE);
        assert!(csignore.contains("AGENTS.md"), "missing seed pattern");

        // .gitignore at the root carries the macOS-friendly defaults.
        let gitignore = std::fs::read_to_string(root.join(".gitignore")).expect("gitignore");
        assert_eq!(gitignore, crate::fs::defaults::GITIGNORE);
        assert!(gitignore.contains(".DS_Store"));
        assert!(gitignore.contains(".contextstore/settings.local.yml"));

        // .contextstore/templates/blank.md seeds new-document creation.
        let blank = std::fs::read_to_string(root.join(".contextstore/templates/blank.md"))
            .expect("blank.md");
        assert_eq!(blank, crate::fs::defaults::BLANK_TEMPLATE);
    }

    #[test]
    fn create_rejects_empty_or_punctuation_only_name() {
        let (_b, _c, manager) = make_manager();
        let err = manager.create("!!!").expect_err("expected error");
        assert!(matches!(err, FsError::Io { .. }));
    }

    #[test]
    fn find_root_returns_none_outside_any_space() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tmp.path().join("not_a_space").join("inner");
        std::fs::create_dir_all(&outside).expect("mkdir outside");

        assert!(SpaceManager::find_root(&outside).is_none());
    }

    // ─── New RegisteredSpace surface ────────────────────────────────

    #[test]
    fn add_registered_path_writes_entry_to_yaml_registry() {
        let (_b, config, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let space_root = external.path().join("DocsRepo");

        manager
            .add_registered_path(RegisteredSpace {
                path: space_root.clone(),
                origin_url: Some("git@github.com:32pixels/docs.git".into()),
                added_at: "2026-06-05T14:23:00Z".into(),
                last_opened_at: None,
            })
            .expect("add");

        let on_disk = std::fs::read_to_string(config.path().join("spaces.yml")).expect("read");
        assert!(
            on_disk.contains("path: "),
            "registry should be YAML with a `path` field: {on_disk}"
        );
        assert!(
            on_disk.contains("origin_url: git@github.com:32pixels/docs.git"),
            "origin_url should round-trip: {on_disk}"
        );
        assert!(
            !on_disk.contains("last_opened_at:"),
            "absent last_opened_at must be omitted, not written as `null`: {on_disk}"
        );
    }

    #[test]
    fn add_registered_path_errors_on_duplicate() {
        let (_b, _c, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let space_root = external.path().join("Twice");

        let entry = RegisteredSpace {
            path: space_root.clone(),
            origin_url: None,
            added_at: "2026-06-05T14:23:00Z".into(),
            last_opened_at: None,
        };
        manager.add_registered_path(entry.clone()).expect("first");
        let err = manager
            .add_registered_path(entry)
            .expect_err("duplicate should error");
        assert!(
            matches!(err, FsError::Io { ref source, .. } if source.kind() == std::io::ErrorKind::AlreadyExists)
        );
    }

    #[test]
    fn is_registered_matches_known_paths() {
        let (_b, _c, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let space_root = external.path().join("Known");
        let other = external.path().join("Other");

        manager
            .add_registered_path(RegisteredSpace {
                path: space_root.clone(),
                origin_url: None,
                added_at: "2026-06-05T14:23:00Z".into(),
                last_opened_at: None,
            })
            .expect("add");

        assert!(manager.is_registered(&space_root).expect("known"));
        assert!(!manager.is_registered(&other).expect("other"));
    }

    #[test]
    fn touch_last_opened_updates_only_matching_entry() {
        let (_b, _c, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let a = external.path().join("A");
        let b = external.path().join("B");

        manager
            .add_registered_path(RegisteredSpace {
                path: a.clone(),
                origin_url: None,
                added_at: "2026-06-05T14:23:00Z".into(),
                last_opened_at: None,
            })
            .expect("add a");
        manager
            .add_registered_path(RegisteredSpace {
                path: b.clone(),
                origin_url: None,
                added_at: "2026-06-05T14:23:00Z".into(),
                last_opened_at: None,
            })
            .expect("add b");

        manager.touch_last_opened(&a).expect("touch a");
        let entries = manager.list_registered();
        let a_entry = entries.iter().find(|e| e.path == a).expect("a present");
        let b_entry = entries.iter().find(|e| e.path == b).expect("b present");
        assert!(
            a_entry.last_opened_at.is_some(),
            "touched entry should set last_opened_at"
        );
        assert!(
            b_entry.last_opened_at.is_none(),
            "untouched entry should remain untouched"
        );
    }

    #[test]
    fn touch_last_opened_is_noop_for_unregistered_path() {
        let (_b, config, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let ghost = external.path().join("Ghost");

        // No registry write, no error, no file created.
        manager.touch_last_opened(&ghost).expect("noop");
        assert!(
            !config.path().join("spaces.yml").exists(),
            "touch on unknown path must not create the registry"
        );
    }

    #[test]
    fn registry_round_trips_byte_stably_for_idempotent_writes() {
        // DoD: "a YAML file with one and many entries round-trips
        // byte-stably (read → no-op write produces identical bytes
        // modulo a trailing newline)."
        let (_b, config, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");

        let entries = vec![
            RegisteredSpace {
                path: external.path().join("One"),
                origin_url: Some("git@github.com:32pixels/one.git".into()),
                added_at: "2026-06-05T14:23:00Z".into(),
                last_opened_at: Some("2026-06-05T15:01:00Z".into()),
            },
            RegisteredSpace {
                path: external.path().join("Two"),
                origin_url: None,
                added_at: "2026-06-04T09:11:00Z".into(),
                last_opened_at: None,
            },
        ];
        manager.write_registry(entries.clone()).expect("write");
        let first = std::fs::read(config.path().join("spaces.yml")).expect("read first");
        manager.write_registry(entries).expect("rewrite");
        let second = std::fs::read(config.path().join("spaces.yml")).expect("read second");
        assert_eq!(first, second, "writes should be byte-stable");
    }

    #[test]
    fn init_at_path_creates_space_at_arbitrary_directory() {
        let (_b, _c, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let root = external.path().join("DocsRepo");

        let space = manager.init_at_path(&root, "Docs Repo").expect("init ok");

        assert_eq!(space.name, "Docs Repo");
        assert_eq!(space.path, root.to_string_lossy());
        assert!(crate::fs::settings::space_settings_path(&root).is_file());
        assert!(root.join(".csignore").is_file());
        assert!(root.join(".gitignore").is_file());
        // Registered.
        assert!(manager.is_registered(&root).expect("is_registered"));
    }

    #[test]
    fn init_at_path_is_idempotent_on_existing_space() {
        let (_b, _c, manager) = make_manager();
        let external = tempfile::tempdir().expect("external tempdir");
        let root = external.path().join("Existing");
        write_space(&root, "Existing");

        let space = manager
            .init_at_path(&root, "Ignored Name")
            .expect("init ok");
        // Existing on-disk name wins; supplied name is ignored.
        assert_eq!(space.name, "Existing");
        // Settings.yml is unchanged — load returns the same struct.
        let loaded = crate::fs::settings::load_space_settings(&root).expect("load");
        assert_eq!(loaded.name, "Existing");
        // Registered exactly once.
        let entries = manager.list_registered();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn an_unreadable_space_does_not_hide_the_readable_ones() {
        // The sandbox failure this guards: a space whose security-scoped
        // bookmark has lapsed can still be stat'd but not read, and treating
        // that as fatal returned an empty list — every other space vanished
        // with it.
        use std::os::unix::fs::PermissionsExt;
        let (_b, _config, manager) = make_manager();
        manager.create("Readable").expect("readable space");
        let outside = tempfile::tempdir().expect("outside");
        let blocked_root = outside.path().join("Blocked");
        manager
            .init_at_path(&blocked_root, "Blocked")
            .expect("blocked space");

        let settings = blocked_root.join(".contextstore").join("settings.yml");
        let mut perms = std::fs::metadata(&settings).expect("meta").permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&settings, perms).expect("chmod");
        // The entry still looks present — only opening it fails.
        assert!(settings.is_file());

        let spaces = manager
            .list()
            .expect("listing survives an unreadable space");
        let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"Readable"),
            "readable space was lost: {names:?}"
        );
        assert!(
            !names.contains(&"Blocked"),
            "unreadable space was listed: {names:?}"
        );

        // Restore permissions so the tempdir can be cleaned up.
        let mut perms = std::fs::metadata(&settings).expect("meta").permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&settings, perms).expect("restore");
    }

    #[test]
    fn registry_reads_yaml_with_optional_fields_omitted() {
        // DoD: "Reading a `spaces.yml` whose entries are missing
        // optional fields (`origin_url`, `last_opened_at`) succeeds
        // and treats them as `None`."
        let (_b, config, manager) = make_manager();
        let on_disk = "\
- path: /tmp/Minimal
  added_at: '2026-06-05T14:23:00Z'
";
        std::fs::write(config.path().join("spaces.yml"), on_disk).expect("seed");

        let entries = manager.list_registered();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("/tmp/Minimal"));
        assert!(entries[0].origin_url.is_none());
        assert!(entries[0].last_opened_at.is_none());
    }
}
