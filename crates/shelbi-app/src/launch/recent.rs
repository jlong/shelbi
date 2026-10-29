//! The recent remote sessions: the remote projects connected to lately,
//! newest first, kept in `<SHELBI_HOME>/recent-remotes.json`.
//!
//! The list holds one entry per session ([`RemoteSession::same_session`]):
//! connecting again moves it to the top with the folder and name it has
//! now. It keeps the [`MAX_RECENT_REMOTES`] newest and drops the rest.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::remote::RemoteSession;

/// The file under the Shelbi home.
pub const RECENT_REMOTES_FILE: &str = "recent-remotes.json";

/// How many sessions the list keeps.
pub const MAX_RECENT_REMOTES: usize = 8;

/// A session connected to before, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentRemote {
    #[serde(flatten)]
    pub session: RemoteSession,
    pub last_connected: DateTime<Utc>,
}

/// The file's shape: a list under a key, so it can grow fields.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RecentFile {
    #[serde(default)]
    sessions: Vec<RecentRemote>,
}

/// Where the recent remote sessions are kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentRemotes {
    /// `None`: nothing is remembered (no Shelbi home).
    path: Option<PathBuf>,
}

impl RecentRemotes {
    /// The list in the Shelbi home.
    pub fn live() -> RecentRemotes {
        RecentRemotes {
            path: shelbi_state::shelbi_home()
                .ok()
                .map(|home| home.join(RECENT_REMOTES_FILE)),
        }
    }

    /// The list in `path`.
    pub fn at(path: impl Into<PathBuf>) -> RecentRemotes {
        RecentRemotes {
            path: Some(path.into()),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The sessions, newest first. Empty when the file is missing or
    /// unreadable (the launch screen never fails over it). A file edited by
    /// hand is put back in order, de-duplicated and capped.
    pub fn load(&self) -> Vec<RecentRemote> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let file: RecentFile = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        normalize(file.sessions)
    }

    /// Remember connecting to `session` at `now`: it moves to the top,
    /// replacing its earlier entry. Returns the list as saved.
    pub fn remember(
        &self,
        session: RemoteSession,
        now: DateTime<Utc>,
    ) -> std::io::Result<Vec<RecentRemote>> {
        let mut sessions = self.load();
        sessions.insert(
            0,
            RecentRemote {
                session,
                last_connected: now,
            },
        );
        self.save(normalize(sessions))
    }

    /// Drop `session` from the list. Returns the list as saved.
    pub fn forget(&self, session: &RemoteSession) -> std::io::Result<Vec<RecentRemote>> {
        let mut sessions = self.load();
        sessions.retain(|r| !r.session.same_session(session));
        self.save(sessions)
    }

    /// Write the list whole, through a temporary file of this process's own
    /// so two clients saving at once never interleave.
    fn save(&self, sessions: Vec<RecentRemote>) -> std::io::Result<Vec<RecentRemote>> {
        let Some(path) = &self.path else {
            return Ok(sessions);
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = RecentFile { sessions };
        let json = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(file.sessions)
    }
}

/// Newest first, one entry per session (the newest wins), at most
/// [`MAX_RECENT_REMOTES`].
fn normalize(mut sessions: Vec<RecentRemote>) -> Vec<RecentRemote> {
    // Stable, so of two entries connected at the same instant the one
    // listed first (the one just remembered) stays first.
    sessions.sort_by_key(|r| std::cmp::Reverse(r.last_connected));
    let mut out: Vec<RecentRemote> = Vec::with_capacity(sessions.len());
    for recent in sessions {
        if !out.iter().any(|kept| kept.session.same_session(&recent.session)) {
            out.push(recent);
        }
    }
    out.truncate(MAX_RECENT_REMOTES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::test_support::Sandbox;
    use chrono::TimeZone;

    fn session(host: &str, project: &str) -> RemoteSession {
        RemoteSession {
            host: host.into(),
            user: "jlong".into(),
            project: project.into(),
            label: None,
            path: format!("~/Workspaces/{project}"),
        }
    }

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 28, 12, minute, 0).unwrap()
    }

    fn projects(list: &[RecentRemote]) -> Vec<String> {
        list.iter()
            .map(|r| format!("{}:{}", r.session.host, r.session.project))
            .collect()
    }

    #[test]
    fn the_live_list_lives_in_the_shelbi_home() {
        let sandbox = Sandbox::new("recent-live");
        let recents = RecentRemotes::live();
        assert_eq!(recents.path(), Some(sandbox.home().join(RECENT_REMOTES_FILE).as_path()));
        assert!(recents.load().is_empty(), "no file yet reads as empty");

        recents.remember(session("devbox", "shelbi"), at(1)).unwrap();
        // A fresh handle reads what the first one saved.
        let reread = RecentRemotes::live().load();
        assert_eq!(projects(&reread), ["devbox:shelbi"]);
        assert_eq!(reread[0].last_connected, at(1));
    }

    #[test]
    fn reconnecting_moves_a_session_to_the_top_without_a_duplicate() {
        let sandbox = Sandbox::new("recent-dedup");
        let recents = RecentRemotes::at(sandbox.home().join("r.json"));
        recents.remember(session("devbox", "shelbi"), at(1)).unwrap();
        recents.remember(session("devbox", "web"), at(2)).unwrap();
        let mut moved = session("devbox", "shelbi");
        moved.path = "/srv/shelbi".into();
        let saved = recents.remember(moved, at(3)).unwrap();

        assert_eq!(projects(&saved), ["devbox:shelbi", "devbox:web"]);
        assert_eq!(saved[0].session.path, "/srv/shelbi", "the newest folder wins");
        assert_eq!(recents.load(), saved);
    }

    #[test]
    fn the_list_keeps_only_the_newest() {
        let sandbox = Sandbox::new("recent-cap");
        let recents = RecentRemotes::at(sandbox.home().join("r.json"));
        for i in 0..(MAX_RECENT_REMOTES as u32 + 3) {
            recents.remember(session("devbox", &format!("p{i}")), at(i)).unwrap();
        }
        let list = recents.load();
        assert_eq!(list.len(), MAX_RECENT_REMOTES);
        assert_eq!(list[0].session.project, format!("p{}", MAX_RECENT_REMOTES + 2));
        assert_eq!(list.last().unwrap().session.project, "p3");
    }

    #[test]
    fn a_hand_edited_or_broken_file_never_fails_the_load() {
        let sandbox = Sandbox::new("recent-broken");
        let path = sandbox.home().join("r.json");
        let recents = RecentRemotes::at(&path);

        std::fs::write(&path, "not json").unwrap();
        assert!(recents.load().is_empty());

        // Out of order and doubled up: read back newest first, once each.
        let file = RecentFile {
            sessions: vec![
                RecentRemote { session: session("devbox", "web"), last_connected: at(1) },
                RecentRemote { session: session("devbox", "shelbi"), last_connected: at(5) },
                RecentRemote { session: session("devbox", "web"), last_connected: at(3) },
            ],
        };
        std::fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();
        let list = recents.load();
        assert_eq!(projects(&list), ["devbox:shelbi", "devbox:web"]);
        assert_eq!(list[1].last_connected, at(3));
    }

    #[test]
    fn forgetting_drops_the_session() {
        let sandbox = Sandbox::new("recent-forget");
        let recents = RecentRemotes::at(sandbox.home().join("r.json"));
        recents.remember(session("devbox", "shelbi"), at(1)).unwrap();
        recents.remember(session("build", "shelbi"), at(2)).unwrap();
        let saved = recents.forget(&session("devbox", "shelbi")).unwrap();
        assert_eq!(projects(&saved), ["build:shelbi"]);
        assert_eq!(recents.load(), saved);
    }

    #[test]
    fn the_file_stores_each_field_by_name() {
        let sandbox = Sandbox::new("recent-shape");
        let path = sandbox.home().join("r.json");
        RecentRemotes::at(&path)
            .remember(session("devbox", "shelbi"), at(7))
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entry = &json["sessions"][0];
        assert_eq!(entry["host"], "devbox");
        assert_eq!(entry["user"], "jlong");
        assert_eq!(entry["project"], "shelbi");
        assert_eq!(entry["path"], "~/Workspaces/shelbi");
        assert_eq!(entry["last_connected"], "2026-10-28T12:07:00Z");
    }
}
