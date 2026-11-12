//! What the desktop app's windows remember, in three parts:
//!
//! - [`WindowState`]: one window's frame, sidebar (width, collapse) and task
//!   pane width.
//! - [`WindowSession`]: the app's window slots, in
//!   `~/.shelbi/desktop-windows.json`. Each slot keeps its window's
//!   [`WindowState`] and the project it last showed, and whether it was open
//!   when the app last ran, so a launch reopens the windows that were open,
//!   each on its project and frame ([`launch_plan`]). Frames belong to the
//!   slot, not the project: a window keeps its frame across project switches.
//! - [`ProjectViewState`]: where a project was left (view, sidebar selection
//!   and scroll, open review, board filter), in
//!   `~/.shelbi/projects/<name>/desktop-view.json` (a remote project's in
//!   `~/.shelbi/remote/<host>/<name>/`, on this machine), so switching back to
//!   a project (in any window) lands where it was.
//!
//! Slots and view state are keyed by [`ProjectKey`], so a remote project named
//! like a local one never shares either. A local key is stored as the bare
//! project name, so session files written before keys read unchanged.
//!
//! Before window slots, the frame was saved per project in
//! `projects/<name>/desktop-window.json` ([`LEGACY_FILE_NAME`]); the first
//! launch without a session file migrates that one to a single window on the
//! launch project.
//!
//! The files are the desktop app's alone. A missing or unreadable file is the
//! default state, never an error: a window must always open.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shelbi_app::nav::DEFAULT_SIDEBAR_WIDTH;
use shelbi_app::source::ProjectKey;

use crate::views::pane_width::DEFAULT_W;

/// The pre-slot per-project window file, read only to migrate it.
pub const LEGACY_FILE_NAME: &str = "desktop-window.json";
/// The window slots, under the Shelbi home.
pub const SESSION_FILE_NAME: &str = "desktop-windows.json";
/// A project's view state, under its state directory.
pub const VIEW_FILE_NAME: &str = "desktop-view.json";

/// A window's frame on screen, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Frame {
    /// This frame placed inside `area` (a display's visible area): exactly
    /// itself when it fits there, a frame filling the whole area included;
    /// otherwise shrunk to fit and moved just far enough to be on it, so a
    /// frame saved on a display that's gone (or at a resolution that no
    /// longer holds it) still opens on screen.
    pub fn fit_within(self, area: Frame) -> Frame {
        let sane = |v: f32| v.is_finite() && v > 0.0;
        if !(sane(area.width) && sane(area.height) && area.x.is_finite() && area.y.is_finite()) {
            return self;
        }
        let width = self.width.min(area.width);
        let height = self.height.min(area.height);
        Frame {
            x: self.x.clamp(area.x, area.x + area.width - width),
            y: self.y.clamp(area.y, area.y + area.height - height),
            width,
            height,
        }
    }
}

/// The display a window's frame was saved on. A frame is relative to its
/// display (gpui reports and places window bounds from the top-left of the
/// window's own screen), so a frame means nothing without it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayRef {
    /// The display's UUID (`CGDisplayCreateUUIDFromDisplayID`): the same
    /// across reboots and reconnects. `None` when it couldn't be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    /// The display's `CGDirectDisplayID`, which can change across reboots and
    /// reconnects (and be reused by another display): matched only when no
    /// UUID was saved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
}

impl DisplayRef {
    /// Whether this is the attached display with `uuid` and `id`: by UUID
    /// when one was saved (a saved display that's gone never matches another
    /// that took its id), else by id.
    pub fn matches(&self, uuid: Option<&str>, id: u64) -> bool {
        match &self.uuid {
            Some(saved) => uuid == Some(saved.as_str()),
            None => self.id == Some(id),
        }
    }
}

/// What a window remembers of itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    /// The window's last frame, relative to [`Self::display`]; `None` until
    /// the window has been moved or sized, so a first open centers a
    /// default-sized window.
    pub frame: Option<Frame>,
    /// The display the frame is on. `None` (a file from before displays were
    /// saved, or a display that couldn't be read) is the primary display.
    pub display: Option<DisplayRef>,
    /// The user's chosen sidebar width, in the shared nav model's column units
    /// (see `crate::views::sidebar::COLUMN_PX`). Saved as chosen, so a window
    /// too narrow to honor it shrinks the sidebar for display only.
    pub sidebar_width: u16,
    /// Whether the sidebar is collapsed.
    pub sidebar_collapsed: bool,
    /// The user's chosen task pane width, in points. Saved as chosen, so a
    /// window too narrow to honor it narrows the pane for display only.
    pub task_pane_width: f32,
    /// The window's project tabs, in order (`crate::project_tabs`): kept
    /// across restarts. Empty in a file from before tabs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<String>,
}

impl Default for WindowState {
    fn default() -> Self {
        WindowState {
            frame: None,
            display: None,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            sidebar_collapsed: false,
            task_pane_width: DEFAULT_W,
            tabs: Vec::new(),
        }
    }
}

impl WindowState {
    /// The pre-slot state file for `project`.
    pub fn legacy_path(project: &str) -> Option<PathBuf> {
        shelbi_state::project_dir(project)
            .ok()
            .map(|dir| dir.join(LEGACY_FILE_NAME))
    }

    /// `project`'s pre-slot window state (what the migration reads), falling
    /// back to the default.
    pub fn load_legacy(project: &str) -> WindowState {
        Self::legacy_path(project)
            .map(|p| load_json(&p))
            .unwrap_or_default()
    }

    /// Load from `path`, falling back to the default for a missing or
    /// malformed file.
    #[cfg(test)]
    pub fn load_from(path: &Path) -> WindowState {
        load_json(path)
    }

    /// Write to `path` atomically.
    #[cfg(test)]
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        write_json(path, self)
    }
}

/// One window slot: its window's state, the project it last showed, and
/// whether the window was open.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Slot {
    /// The project the window last showed (`None`: the empty-state chooser).
    pub project: Option<ProjectKey>,
    /// The window was open when the app last ran (a closed slot keeps its
    /// frame for the next window that takes it).
    pub open: bool,
    #[serde(flatten)]
    pub state: WindowState,
}

/// The app's window slots.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSession {
    pub slots: Vec<Slot>,
}

impl WindowSession {
    /// The session file, or `None` with no Shelbi home.
    pub fn path() -> Option<PathBuf> {
        shelbi_state::shelbi_home()
            .ok()
            .map(|home| home.join(SESSION_FILE_NAME))
    }

    /// The saved session; `None` when there is no file yet (the first launch
    /// after the upgrade, which migrates).
    pub fn load() -> Option<WindowSession> {
        let path = Self::path()?;
        let bytes = std::fs::read(path).ok()?;
        Some(serde_json::from_slice(&bytes).unwrap_or_default())
    }

    /// Save it. Blocking disk IO: call it off the UI thread.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| std::io::Error::other("no Shelbi home"))?;
        write_json(&path, self)
    }

    /// The lowest slot no open window holds, among `taken`.
    pub fn free_slot(taken: &[usize]) -> usize {
        (0..).find(|s| !taken.contains(s)).unwrap_or(0)
    }

    /// Record `slot` (growing the list to hold it).
    pub fn set(&mut self, slot: usize, entry: Slot) {
        if self.slots.len() <= slot {
            self.slots.resize(slot + 1, Slot::default());
        }
        self.slots[slot] = entry;
    }

    /// Mark `slot` closed (its window was closed while others stayed open).
    pub fn close(&mut self, slot: usize) {
        if let Some(s) = self.slots.get_mut(slot) {
            s.open = false;
        }
    }
}

/// A window to open at launch.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchWindow {
    pub slot: usize,
    pub project: ProjectKey,
    pub state: WindowState,
}

/// The windows a launch opens:
///
/// - With a session, every slot that was open on a project that still
///   exists (`exists`), each on its project and state. A project named on
///   the command line (`named`, on this machine) takes the first window
///   unless one already shows it.
/// - Without one (the first launch after the upgrade), one window on `named`
///   or else `fallback` (the launch project), with that project's pre-slot
///   frame (`legacy`).
/// - With no open slot left, one window on `named` or `fallback`, in slot 0.
///
/// Empty only when there is no project at all.
pub fn launch_plan(
    session: Option<&WindowSession>,
    named: Option<&str>,
    fallback: Option<&str>,
    exists: &dyn Fn(&ProjectKey) -> bool,
    legacy: &dyn Fn(&str) -> WindowState,
) -> Vec<LaunchWindow> {
    let named = named.map(ProjectKey::local);
    let Some(session) = session else {
        return named
            .map(|n| n.name)
            .or(fallback.map(str::to_string))
            .map(|project| LaunchWindow {
                slot: 0,
                state: legacy(&project),
                project: ProjectKey::local(project),
            })
            .into_iter()
            .collect();
    };
    let mut windows: Vec<LaunchWindow> = session
        .slots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.open)
        .filter_map(|(slot, s)| {
            let project = s.project.as_ref().filter(|p| exists(p))?;
            Some(LaunchWindow {
                slot,
                project: project.clone(),
                state: s.state.clone(),
            })
        })
        .collect();
    let shows_named = named.as_ref().is_some_and(|n| windows.iter().any(|w| w.project == *n));
    match (windows.first_mut(), named) {
        (Some(first), Some(named)) if !shows_named => first.project = named,
        (Some(_), _) => {}
        (None, named) => {
            if let Some(project) = named.or(fallback.map(ProjectKey::local)) {
                windows.push(LaunchWindow {
                    slot: 0,
                    project,
                    state: session.slots.first().map(|s| s.state.clone()).unwrap_or_default(),
                });
            }
        }
    }
    windows
}

/// Where a project was left, so switching back lands there.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectViewState {
    /// The main area's view as its `view:<id>` suffix (`orch`, `tasks`, a
    /// workspace's name). `None`: the default, the orchestrator chat. A
    /// review shown over a view keeps that view here and the task in
    /// [`Self::review`].
    pub view: Option<String>,
    /// Keys were on the sidebar (else the main area).
    pub sidebar_focused: bool,
    /// The selected sidebar row, by what it opens (`view:tasks`,
    /// `workspace:alpha`, `review:T-1`, `machine:hub`), so it survives rows
    /// coming and going.
    pub selected: Option<String>,
    /// The sidebar's scroll offset (gpui's: zero or negative).
    pub scroll_y: f32,
    /// The review that was open.
    pub review: Option<String>,
    /// The view the shown workspace was opened over (its panel's Back).
    pub workspace_return: Option<String>,
    /// The board's workflow filter (its workspace filter is saved by the
    /// board itself, shared with the TUI).
    pub workflow_filter: Option<String>,
    /// The sidebar sections collapsed (`ready`, `queued`, `workspaces`).
    pub collapsed_sections: Vec<String>,
}

impl ProjectViewState {
    /// Where `project`'s is kept: under its state directory on this machine
    /// ([`ProjectKey::state_dir`]).
    pub fn path(project: &ProjectKey) -> Option<PathBuf> {
        project.state_dir().ok().map(|dir| dir.join(VIEW_FILE_NAME))
    }

    /// `project`'s saved view state, or the default.
    pub fn load(project: &ProjectKey) -> ProjectViewState {
        Self::path(project).map(|p| load_json(&p)).unwrap_or_default()
    }

    /// Save it for `project`. Blocking disk IO: call it off the UI thread.
    pub fn save(&self, project: &ProjectKey) -> std::io::Result<()> {
        let path = Self::path(project)
            .ok_or_else(|| std::io::Error::other(format!("no state directory for `{project}`")))?;
        write_json(&path, self)
    }
}

/// Read `path` as JSON, the default for a missing or malformed file.
fn load_json<T: Default + serde::de::DeserializeOwned>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Write `value` to `path` atomically (a temp file renamed over the target),
/// so a crash mid-write never leaves a torn file. One write at a time: two
/// windows saving at once never share a temp file.
fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITING.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
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
        let path = temp_dir("rt").join("demo").join(LEGACY_FILE_NAME);
        let state = WindowState {
            frame: Some(Frame {
                x: 40.0,
                y: 60.0,
                width: 1280.0,
                height: 820.0,
            }),
            display: Some(DisplayRef {
                uuid: Some("37D8832A-2D66-02CA-B9F7-8F30A301B230".into()),
                id: Some(2),
            }),
            sidebar_width: 33,
            sidebar_collapsed: true,
            task_pane_width: 812.0,
            tabs: vec!["web".into(), "api".into()],
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
        let bad = dir.join(LEGACY_FILE_NAME);
        std::fs::write(&bad, b"{ not json").unwrap();
        assert_eq!(WindowState::load_from(&bad), WindowState::default());
        // A partial file (an older build's) keeps what it has.
        std::fs::write(&bad, br#"{"sidebar_width": 30}"#).unwrap();
        let partial = WindowState::load_from(&bad);
        assert_eq!(partial.sidebar_width, 30);
        assert_eq!(partial.frame, None);
        assert!(!partial.sidebar_collapsed);
        assert_eq!(partial.task_pane_width, DEFAULT_W, "a file from before the pane resized");
    }

    fn frame(x: f32, y: f32, width: f32, height: f32) -> Frame {
        Frame { x, y, width, height }
    }

    #[test]
    fn a_frame_that_fits_the_visible_area_is_kept_exactly() {
        // A 1710x1112pt laptop display under a 39pt menu bar.
        let area = frame(0.0, 39.0, 1710.0, 1073.0);
        // Filling the whole visible area (AppKit reports the left edge as -0.0).
        let full = frame(-0.0, 39.0, 1710.0, 1073.0);
        assert_eq!(full.fit_within(area), full);
        let small = frame(200.0, 150.0, 1200.0, 800.0);
        assert_eq!(small.fit_within(area), small);
        // Touching the bottom-right corner.
        let corner = frame(710.0, 112.0, 1000.0, 1000.0);
        assert_eq!(corner.fit_within(area), corner);
        // And it survives the file the way it's saved.
        let json = serde_json::to_string(&full).unwrap();
        assert_eq!(json, r#"{"x":-0.0,"y":39.0,"width":1710.0,"height":1073.0}"#);
        let back: Frame = serde_json::from_str(&json).unwrap();
        assert_eq!(back.fit_within(area), full);
    }

    #[test]
    fn a_frame_that_no_longer_fits_is_pulled_onto_the_visible_area() {
        let area = frame(0.0, 39.0, 1710.0, 1073.0);
        // Saved on a display to the right that's gone: moved on, size kept.
        assert_eq!(
            frame(2000.0, 300.0, 900.0, 700.0).fit_within(area),
            frame(810.0, 300.0, 900.0, 700.0)
        );
        // Above and left of it.
        assert_eq!(
            frame(-500.0, -200.0, 900.0, 700.0).fit_within(area),
            frame(0.0, 39.0, 900.0, 700.0)
        );
        // Saved on a bigger display: shrunk to the area.
        assert_eq!(
            frame(-0.0, 25.0, 2560.0, 1400.0).fit_within(area),
            frame(0.0, 39.0, 1710.0, 1073.0)
        );
        // Half off the bottom: moved up, size kept.
        assert_eq!(
            frame(100.0, 800.0, 1000.0, 600.0).fit_within(area),
            frame(100.0, 512.0, 1000.0, 600.0)
        );
        // A degenerate area (no display read) leaves the frame alone.
        let any = frame(5000.0, 5000.0, 900.0, 700.0);
        assert_eq!(any.fit_within(frame(0.0, 0.0, 0.0, 0.0)), any);
    }

    fn state(x: f32) -> WindowState {
        WindowState {
            frame: Some(Frame {
                x,
                y: 10.0,
                width: 900.0,
                height: 700.0,
            }),
            display: None,
            sidebar_width: 30,
            sidebar_collapsed: false,
            task_pane_width: DEFAULT_W,
            tabs: Vec::new(),
        }
    }

    fn slot(project: &str, open: bool, x: f32) -> Slot {
        Slot {
            project: Some(ProjectKey::local(project)),
            open,
            state: state(x),
        }
    }

    fn all_exist(_: &ProjectKey) -> bool {
        true
    }

    #[test]
    fn the_first_launch_after_the_upgrade_migrates_to_one_window() {
        // No session file: one window on the launch project, with the frame
        // its old per-project file saved.
        let legacy = |p: &str| if p == "web" { state(40.0) } else { WindowState::default() };
        let plan = launch_plan(None, None, Some("web"), &all_exist, &legacy);
        assert_eq!(
            plan,
            [LaunchWindow {
                slot: 0,
                project: ProjectKey::local("web"),
                state: state(40.0),
            }]
        );
        // A project named on the command line wins over the launch project.
        let plan = launch_plan(None, Some("api"), Some("web"), &all_exist, &legacy);
        assert_eq!(plan[0].project, ProjectKey::local("api"));
        assert_eq!(plan[0].state, WindowState::default());
        // No project at all: nothing to open.
        assert!(launch_plan(None, None, None, &all_exist, &legacy).is_empty());
    }

    #[test]
    fn a_launch_restores_the_open_windows_each_on_its_project() {
        let session = WindowSession {
            slots: vec![
                slot("web", true, 10.0),
                // Closed while others stayed open: not restored.
                slot("api", false, 20.0),
                slot("docs", true, 30.0),
                // Its project was removed since.
                slot("gone", true, 40.0),
            ],
        };
        let exists = |p: &ProjectKey| p.name != "gone";
        let never = |_: &str| -> WindowState { panic!("a session never reads the legacy file") };
        let plan = launch_plan(Some(&session), None, Some("api"), &exists, &never);
        assert_eq!(
            plan,
            [
                LaunchWindow {
                    slot: 0,
                    project: ProjectKey::local("web"),
                    state: state(10.0),
                },
                LaunchWindow {
                    slot: 2,
                    project: ProjectKey::local("docs"),
                    state: state(30.0),
                },
            ]
        );
        // A named project takes the first window unless one shows it.
        let plan = launch_plan(Some(&session), Some("api"), None, &exists, &never);
        assert_eq!(plan[0].project, ProjectKey::local("api"));
        assert_eq!(plan[0].state, state(10.0), "the window keeps its slot's frame");
        let plan = launch_plan(Some(&session), Some("docs"), None, &exists, &never);
        assert_eq!(plan[0].project, ProjectKey::local("web"));
        // Nothing left open: one window, in slot 0's frame.
        let closed = WindowSession {
            slots: vec![slot("web", false, 10.0)],
        };
        let plan = launch_plan(Some(&closed), None, Some("api"), &exists, &never);
        assert_eq!(
            plan,
            [LaunchWindow {
                slot: 0,
                project: ProjectKey::local("api"),
                state: state(10.0),
            }]
        );
    }

    #[test]
    fn slots_are_reused_lowest_first_and_round_trip() {
        assert_eq!(WindowSession::free_slot(&[]), 0);
        assert_eq!(WindowSession::free_slot(&[0, 1, 3]), 2);
        let mut session = WindowSession::default();
        session.set(2, slot("web", true, 10.0));
        assert_eq!(session.slots.len(), 3);
        assert_eq!(session.slots[0], Slot::default());
        session.close(2);
        assert!(!session.slots[2].open);
        assert_eq!(session.slots[2].state, state(10.0), "a closed slot keeps its frame");
        let path = temp_dir("session").join(SESSION_FILE_NAME);
        write_json(&path, &session).unwrap();
        assert_eq!(load_json::<WindowSession>(&path), session);
    }

    #[test]
    fn view_state_round_trips_and_defaults() {
        let dir = temp_dir("view");
        let path = dir.join(VIEW_FILE_NAME);
        assert_eq!(load_json::<ProjectViewState>(&path), ProjectViewState::default());
        let view = ProjectViewState {
            view: Some("alpha-1".into()),
            sidebar_focused: true,
            selected: Some("workspace:alpha-1".into()),
            scroll_y: -48.0,
            review: Some("T-9".into()),
            workspace_return: Some("tasks".into()),
            workflow_filter: Some("task".into()),
            collapsed_sections: vec!["queued".into()],
        };
        write_json(&path, &view).unwrap();
        assert_eq!(load_json::<ProjectViewState>(&path), view);
    }

    #[test]
    fn default_width_is_the_shared_nav_default() {
        assert_eq!(WindowState::default().sidebar_width, DEFAULT_SIDEBAR_WIDTH);
        assert_eq!(WindowState::default().task_pane_width, 640.0);
    }

    #[test]
    fn slots_key_projects_and_read_a_bare_name_as_a_local_one() {
        // A session file from before keys: the bare name is a local project.
        let old = r#"{"slots":[{"project":"web","open":true}]}"#;
        let session: WindowSession = serde_json::from_str(old).unwrap();
        assert_eq!(session.slots[0].project, Some(ProjectKey::local("web")));
        // A local key writes back the same bare name.
        let json = serde_json::to_value(&session).unwrap();
        assert_eq!(json["slots"][0]["project"], "web");
        // A remote one keeps its host, and is another project than the
        // local one of the same name.
        let mut remote = session.clone();
        remote.slots[0].project = Some(ProjectKey::remote("devbox", "web"));
        let back: WindowSession = serde_json::from_str(&serde_json::to_string(&remote).unwrap()).unwrap();
        assert_eq!(back, remote);
        assert_ne!(back.slots[0].project, session.slots[0].project);
        // A slot whose project isn't there to open (a remote one, until
        // remote windows open natively) isn't restored.
        let local_only = |p: &ProjectKey| p.is_local();
        let never = |_: &str| -> WindowState { panic!("a session never reads the legacy file") };
        assert!(launch_plan(Some(&remote), None, None, &local_only, &never).is_empty());
    }

    #[test]
    fn a_saved_display_matches_by_uuid_else_by_id() {
        let uuid = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
        let saved = DisplayRef {
            uuid: Some(uuid.into()),
            id: Some(2),
        };
        assert!(saved.matches(Some(uuid), 2));
        // The same display under a new id (after a reboot or reconnect).
        assert!(saved.matches(Some(uuid), 7));
        // Another display that took its id: not it.
        assert!(!saved.matches(Some("0F6A2B1C-0000-0000-0000-000000000001"), 2));
        assert!(!saved.matches(None, 2));
        // No UUID was read when it was saved: the id is all there is.
        let by_id = DisplayRef { uuid: None, id: Some(2) };
        assert!(by_id.matches(Some(uuid), 2));
        assert!(!by_id.matches(Some(uuid), 3));
        assert!(!DisplayRef::default().matches(Some(uuid), 2));
    }

    #[test]
    fn a_session_from_before_displays_reads_as_the_primary() {
        // Written before displays were saved: no display, which opens on the
        // primary display.
        let old = r#"{"slots":[{"project":"web","open":true,
            "frame":{"x":40.0,"y":60.0,"width":1280.0,"height":820.0},
            "sidebar_width":30}]}"#;
        let session: WindowSession = serde_json::from_str(old).unwrap();
        let state = &session.slots[0].state;
        assert_eq!(state.display, None);
        assert_eq!(state.frame.map(|f| f.width), Some(1280.0));
        assert_eq!(state.sidebar_width, 30);
        // A display is saved beside the frame and reads back.
        let mut session = session;
        session.slots[0].state.display = Some(DisplayRef {
            uuid: Some("37D8832A-2D66-02CA-B9F7-8F30A301B230".into()),
            id: Some(2),
        });
        let json = serde_json::to_value(&session).unwrap();
        assert_eq!(json["slots"][0]["display"]["uuid"], "37D8832A-2D66-02CA-B9F7-8F30A301B230");
        assert_eq!(json["slots"][0]["display"]["id"], 2);
        let back: WindowSession = serde_json::from_value(json).unwrap();
        assert_eq!(back, session);
    }
}
