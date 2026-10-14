//! Per-project window state: size, position, and the sidebar's width and
//! collapse, persisted to `~/.shelbi/projects/<name>/desktop-window.json` so a
//! project's window reopens where the user left it.
//!
//! The file is the desktop app's alone (the TUI's sidebar width is per-client
//! and in memory). A missing or unreadable file is the default state, never
//! an error: the window must always open.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shelbi_app::nav::DEFAULT_SIDEBAR_WIDTH;

/// The file name under the project's state directory.
pub const FILE_NAME: &str = "desktop-window.json";

/// A window's frame on screen, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// What a project's window remembers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    /// The window's last frame; `None` until the window has been moved or
    /// sized, so a first open centers a default-sized window.
    pub frame: Option<Frame>,
    /// The user's chosen sidebar width, in the shared nav model's column units
    /// (see `crate::views::sidebar::COLUMN_PX`). Saved as chosen, so a window
    /// too narrow to honor it shrinks the sidebar for display only.
    pub sidebar_width: u16,
    /// Whether the sidebar is collapsed.
    pub sidebar_collapsed: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        WindowState {
            frame: None,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            sidebar_collapsed: false,
        }
    }
}

impl WindowState {
    /// The state file for `project`, or `None` when the project name is
    /// invalid or there's no Shelbi home.
    pub fn path(project: &str) -> Option<PathBuf> {
        shelbi_state::project_dir(project)
            .ok()
            .map(|dir| dir.join(FILE_NAME))
    }

    /// Load `project`'s window state, falling back to the default.
    pub fn load(project: &str) -> WindowState {
        Self::path(project)
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    /// Load from `path`, falling back to the default for a missing or
    /// malformed file.
    pub fn load_from(path: &Path) -> WindowState {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Save `project`'s window state. Blocking disk IO: call it off the UI
    /// thread.
    pub fn save(&self, project: &str) -> std::io::Result<()> {
        let path = Self::path(project)
            .ok_or_else(|| std::io::Error::other(format!("no state directory for `{project}`")))?;
        self.save_to(&path)
    }

    /// Write to `path` atomically (a temp file renamed over the target), so a
    /// crash mid-write never leaves a torn file.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-desktop-window-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_through_the_file() {
        let path = temp_dir("rt").join("demo").join(FILE_NAME);
        let state = WindowState {
            frame: Some(Frame {
                x: 40.0,
                y: 60.0,
                width: 1280.0,
                height: 820.0,
            }),
            sidebar_width: 33,
            sidebar_collapsed: true,
        };
        state.save_to(&path).unwrap();
        assert_eq!(WindowState::load_from(&path), state);
        // No temp file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn missing_or_malformed_files_are_the_default() {
        let dir = temp_dir("bad");
        assert_eq!(
            WindowState::load_from(&dir.join("absent.json")),
            WindowState::default()
        );
        let bad = dir.join(FILE_NAME);
        std::fs::write(&bad, b"{ not json").unwrap();
        assert_eq!(WindowState::load_from(&bad), WindowState::default());
        // A partial file (an older build's) keeps what it has.
        std::fs::write(&bad, br#"{"sidebar_width": 30}"#).unwrap();
        let partial = WindowState::load_from(&bad);
        assert_eq!(partial.sidebar_width, 30);
        assert_eq!(partial.frame, None);
        assert!(!partial.sidebar_collapsed);
    }

    #[test]
    fn default_width_is_the_shared_nav_default() {
        assert_eq!(WindowState::default().sidebar_width, DEFAULT_SIDEBAR_WIDTH);
    }
}
