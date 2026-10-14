//! The new-release check: once a day, off the UI thread, ask GitHub for the
//! latest Shelbi release and, when it is newer than this app, show a notice
//! with the Homebrew command that upgrades it. There is no in-app updater:
//! the app ships as a Homebrew cask (`shelbi-desktop`) that depends on the
//! `shelbi` formula, and Homebrew owns the upgrade.
//!
//! Rate limit: the result is cached in `~/.shelbi/desktop-update-check.json`
//! (`checked_at` + `latest`) and GitHub is asked at most once per
//! [`INTERVAL`], whether the last ask succeeded or not; inside the interval
//! the cached answer still drives the notice, so it survives a relaunch.
//!
//! Overrides (both read once, at launch):
//!
//! - `SHELBI_DESKTOP_UPDATE_URL`: ask this URL instead (any URL `curl`
//!   takes, `file://` included). It bypasses the cache in both directions, so
//!   pointing at a fixture never waits a day and never pollutes the real
//!   cache. `desktop/README.md` shows how to test the notice this way.
//! - `SHELBI_DESKTOP_UPDATE_CHECK=0` (or `off`, `false`): never check.
//!
//! The fetch is `/usr/bin/curl`: present on every Mac, and it keeps an HTTP
//! stack out of the app for one GET a day.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::version::{self, APP_VERSION};

/// The GitHub releases API endpoint for the newest non-draft, non-prerelease
/// release.
pub const RELEASES_URL: &str = "https://api.github.com/repos/jlong/shelbi/releases/latest";
/// How often GitHub is asked.
pub const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// The cache file, in the Shelbi home.
pub const CACHE_FILE: &str = "desktop-update-check.json";

const URL_ENV: &str = "SHELBI_DESKTOP_UPDATE_URL";
const DISABLE_ENV: &str = "SHELBI_DESKTOP_UPDATE_CHECK";
const FETCH_TIMEOUT_SECS: &str = "10";

/// A newer release exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    pub current: String,
    pub latest: String,
}

impl UpdateNotice {
    pub fn message(&self) -> String {
        format!(
            "Shelbi {} is available (you have {}).",
            self.latest, self.current
        )
    }
}

/// What was learned at `checked_at` (seconds since the epoch).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cache {
    pub checked_at: u64,
    /// The latest release's version (no leading `v`), when the last
    /// successful ask returned one. Kept across a failed ask.
    pub latest: Option<String>,
}

/// Where the check asks, and whether it may use the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// GitHub, at most once per [`INTERVAL`], through the cache at this path
    /// (`None`: no Shelbi home, so no cache; ask every launch).
    Releases { cache: Option<PathBuf> },
    /// `SHELBI_DESKTOP_UPDATE_URL`: always asked, never cached.
    Override(String),
}

/// The check's configuration from the environment; `None` when disabled.
pub fn source_from_env(var: impl Fn(&str) -> Option<String>) -> Option<Source> {
    if let Some(v) = var(DISABLE_ENV) {
        if matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        ) {
            return None;
        }
    }
    match var(URL_ENV).filter(|u| !u.trim().is_empty()) {
        Some(url) => Some(Source::Override(url)),
        None => Some(Source::Releases {
            cache: shelbi_state::shelbi_home().ok().map(|h| h.join(CACHE_FILE)),
        }),
    }
}

/// Run the check against the real machine (blocking: call it off the UI
/// thread). `None` when there's nothing to show.
pub fn check_now() -> Option<UpdateNotice> {
    let source = source_from_env(|k| std::env::var(k).ok())?;
    check(&source, APP_VERSION, now_secs(), fetch_with_curl)
}

/// The check, with the clock and the network injected.
pub fn check(
    source: &Source,
    app: &str,
    now: u64,
    fetch: impl FnOnce(&str) -> Result<String, String>,
) -> Option<UpdateNotice> {
    let latest = match source {
        Source::Override(url) => match fetch(url).and_then(|b| latest_from_release_json(&b)) {
            Ok(latest) => latest,
            Err(e) => {
                tracing::warn!(%url, "update check failed: {e}");
                None
            }
        },
        Source::Releases { cache } => {
            let cached = cache.as_deref().and_then(read_cache);
            match plan(now, cached.as_ref()) {
                Plan::UseCache(latest) => latest,
                Plan::Fetch => {
                    let latest =
                        match fetch(RELEASES_URL).and_then(|b| latest_from_release_json(&b)) {
                            Ok(latest) => latest,
                            Err(e) => {
                                tracing::info!("update check failed (next try in a day): {e}");
                                cached.and_then(|c| c.latest)
                            }
                        };
                    if let Some(path) = cache {
                        let entry = Cache {
                            checked_at: now,
                            latest: latest.clone(),
                        };
                        if let Err(e) = write_cache(path, &entry) {
                            tracing::warn!(path = %path.display(), "could not save the update check: {e}");
                        }
                    }
                    latest
                }
            }
        }
    };
    newer(app, latest.as_deref()?)
}

/// Whether to ask GitHub now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Fetch,
    /// Asked within the interval: this is what it said.
    UseCache(Option<String>),
}

pub fn plan(now: u64, cache: Option<&Cache>) -> Plan {
    match cache {
        // A `checked_at` in the future (the clock moved back) doesn't block
        // checks forever.
        Some(c) if c.checked_at <= now && now - c.checked_at < INTERVAL.as_secs() => {
            Plan::UseCache(c.latest.clone())
        }
        _ => Plan::Fetch,
    }
}

/// A notice when `latest` is a newer version than `app`.
pub fn newer(app: &str, latest: &str) -> Option<UpdateNotice> {
    let latest = latest.trim().trim_start_matches('v');
    (version::compare(latest, app) == Some(std::cmp::Ordering::Greater)).then(|| UpdateNotice {
        current: app.to_string(),
        latest: latest.to_string(),
    })
}

/// The version in a GitHub release JSON object (`tag_name`, `v` stripped).
/// `Ok(None)` for a draft or prerelease (the `latest` endpoint returns
/// neither, but a fixture might); `Err` when it isn't a release object.
pub fn latest_from_release_json(body: &str) -> Result<Option<String>, String> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
    }
    let release: Release =
        serde_json::from_str(body).map_err(|e| format!("not a GitHub release: {e}"))?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    Ok(Some(release.tag_name.trim_start_matches('v').to_string()))
}

fn read_cache(path: &Path) -> Option<Cache> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_cache(path: &Path, cache: &Cache) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(cache)?)?;
    std::fs::rename(tmp, path)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn fetch_with_curl(url: &str) -> Result<String, String> {
    let out = Command::new("/usr/bin/curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args(["--max-time", FETCH_TIMEOUT_SECS])
        .args(["--header", "Accept: application/vnd.github+json"])
        .args(["--header", "X-GitHub-Api-Version: 2022-11-28"])
        .args(["--user-agent", &format!("shelbi-desktop/{APP_VERSION}")])
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const DAY: u64 = 24 * 60 * 60;

    fn release(tag: &str) -> String {
        format!(r#"{{"tag_name":"{tag}","draft":false,"prerelease":false,"name":"x"}}"#)
    }

    fn temp_cache(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-desktop-update-{tag}-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join(CACHE_FILE)
    }

    #[test]
    fn a_newer_release_is_a_notice_and_an_equal_or_older_one_is_silent() {
        assert_eq!(
            newer("0.10.0", "v0.11.0"),
            Some(UpdateNotice {
                current: "0.10.0".into(),
                latest: "0.11.0".into()
            })
        );
        assert_eq!(newer("0.10.0", "v0.10.0"), None);
        assert_eq!(newer("0.10.0", "0.9.9"), None);
        assert_eq!(newer("0.10.0", "garbage"), None);
    }

    #[test]
    fn release_json_reads_the_tag_and_skips_drafts_and_prereleases() {
        assert_eq!(
            latest_from_release_json(&release("v1.2.3")),
            Ok(Some("1.2.3".into()))
        );
        assert_eq!(
            latest_from_release_json(r#"{"tag_name":"v2.0.0","prerelease":true}"#),
            Ok(None)
        );
        assert!(latest_from_release_json(r#"{"message":"Not Found"}"#).is_err());
    }

    #[test]
    fn asks_at_most_once_a_day() {
        let at = |checked_at| Cache {
            checked_at,
            latest: Some("0.11.0".into()),
        };
        assert_eq!(plan(10 * DAY, None), Plan::Fetch);
        assert_eq!(
            plan(10 * DAY, Some(&at(10 * DAY - 60))),
            Plan::UseCache(Some("0.11.0".into()))
        );
        assert_eq!(plan(10 * DAY, Some(&at(9 * DAY))), Plan::Fetch);
        // The clock went backwards past the last check: ask again.
        assert_eq!(plan(10 * DAY, Some(&at(11 * DAY))), Plan::Fetch);
    }

    #[test]
    fn the_cached_answer_drives_the_notice_without_a_fetch() {
        let path = temp_cache("cached");
        let source = Source::Releases {
            cache: Some(path.clone()),
        };
        let fetched = Cell::new(0);
        let fetch = |_: &str| {
            fetched.set(fetched.get() + 1);
            Ok(release("v0.11.0"))
        };
        assert!(check(&source, "0.10.0", 100 * DAY, fetch).is_some());
        assert_eq!(fetched.get(), 1);
        // An hour later: no fetch, same notice.
        let notice = check(&source, "0.10.0", 100 * DAY + 3600, |_: &str| {
            panic!("fetched inside the interval")
        });
        assert_eq!(notice.map(|n| n.latest), Some("0.11.0".to_string()));
        // A day later it asks again.
        assert!(check(&source, "0.10.0", 101 * DAY, fetch).is_some());
        assert_eq!(fetched.get(), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_failed_fetch_counts_as_the_days_check_and_keeps_the_last_answer() {
        let path = temp_cache("failed");
        let source = Source::Releases {
            cache: Some(path.clone()),
        };
        check(&source, "0.10.0", 100 * DAY, |_: &str| {
            Ok(release("v0.11.0"))
        });
        let notice = check(&source, "0.10.0", 101 * DAY, |_: &str| {
            Err("offline".to_string())
        });
        assert_eq!(notice.map(|n| n.latest), Some("0.11.0".to_string()));
        assert_eq!(read_cache(&path).map(|c| c.checked_at), Some(101 * DAY));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_override_url_is_always_asked_and_never_cached() {
        let fixture = std::env::temp_dir().join(format!(
            "shelbi-desktop-update-fixture-{}.json",
            std::process::id()
        ));
        std::fs::write(&fixture, release("v99.0.0")).unwrap();
        let url = format!("file://{}", fixture.display());
        // Through the real curl, the way `SHELBI_DESKTOP_UPDATE_URL` is used.
        let notice = check(&Source::Override(url.clone()), "0.10.0", 0, fetch_with_curl);
        assert_eq!(notice.map(|n| n.latest), Some("99.0.0".to_string()));
        std::fs::write(&fixture, release("v0.10.0")).unwrap();
        assert_eq!(
            check(&Source::Override(url), "0.10.0", 0, fetch_with_curl),
            None
        );
        let _ = std::fs::remove_file(fixture);
    }

    #[test]
    fn the_environment_picks_the_source() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(source_from_env(env(&[(DISABLE_ENV, "0")])), None);
        assert_eq!(
            source_from_env(env(&[(DISABLE_ENV, "off"), (URL_ENV, "x")])),
            None
        );
        assert_eq!(
            source_from_env(env(&[(URL_ENV, "file:///tmp/r.json")])),
            Some(Source::Override("file:///tmp/r.json".into()))
        );
    }
}
