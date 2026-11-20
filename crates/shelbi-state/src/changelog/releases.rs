//! A project's releases, for the Changelog page's timeline: the git tags in
//! its repository that match the `releases.tags` glob of its YAML
//! ([`shelbi_core::releases`]), each with a label and a date.
//!
//! A release's date is the tag's creation date: the tagger date of an
//! annotated tag, the commit date of a lightweight one (git's
//! `creatordate`). Tags come from the local repository only; nothing fetches.
//!
//! Reading runs `git for-each-ref`, so a client calls [`read_releases`] off
//! its UI thread (the changelog read does). The tag list is cached per
//! repository against a stamp of its tag refs (the loose refs under
//! `refs/tags/` and `packed-refs`), so a re-read with no new tag costs a few
//! `stat`s and no process.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::SystemTime;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use shelbi_core::{MachineKind, Project, ReleasesConfig};

use crate::expand_tilde_str;

/// One release on the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// The git tag (`v0.14.0`, `testflight/1.2.0-1`).
    pub tag: String,
    /// How it reads: the `releases.label` template filled for the tag.
    pub label: String,
    /// When the tag was made (tagger date, or the commit's for a lightweight
    /// tag), in the offset it was made in.
    pub date: DateTime<FixedOffset>,
}

/// One read of a project's releases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseRead {
    /// Newest first. Empty with no `releases:` block.
    pub releases: Vec<Release>,
    /// Why the releases couldn't be read: a malformed `releases:` block
    /// (`releases.tags \`v[0-9\`: …`) or a repository git can't list.
    pub error: Option<String>,
}

/// `project`'s releases. A project that won't load, or has no `releases:`
/// block, has none and no error: the timeline then shows its ticks alone.
pub fn read_releases(project: &str) -> ReleaseRead {
    match crate::load_project(project) {
        Ok(p) => read_releases_of(&p),
        Err(_) => ReleaseRead::default(),
    }
}

/// [`read_releases`] for a loaded project.
pub fn read_releases_of(project: &Project) -> ReleaseRead {
    let Some(config) = &project.releases else {
        return ReleaseRead::default();
    };
    read_releases_in(config, &local_checkout(project))
}

/// The project's repository on this machine: `repo:` when it's a directory
/// here, else the local hub machine's `work_dir` (a project whose `repo:`
/// records the remote's URL keeps its checkout there).
pub fn local_checkout(project: &Project) -> PathBuf {
    let repo = expand_tilde_str(&project.repo);
    if repo.is_dir() {
        return repo;
    }
    project
        .machines
        .iter()
        .find(|m| matches!(m.kind, MachineKind::Local))
        .map(|m| expand_tilde_str(&m.work_dir.to_string_lossy()))
        .unwrap_or(repo)
}

/// The releases `config` finds among the tags of the repository at `repo`.
pub fn read_releases_in(config: &ReleasesConfig, repo: &Path) -> ReleaseRead {
    let glob = match config.validate() {
        Ok(glob) => glob,
        Err(e) => {
            return ReleaseRead {
                releases: Vec::new(),
                error: Some(e.to_string()),
            }
        }
    };
    let tags = match repo_tags(repo) {
        Ok(tags) => tags,
        Err(e) => {
            return ReleaseRead {
                releases: Vec::new(),
                error: Some(e),
            }
        }
    };
    let mut releases: Vec<Release> = tags
        .iter()
        .filter(|t| glob.matches(&t.name))
        .map(|t| Release {
            tag: t.name.clone(),
            label: config.label_for(&glob, &t.name),
            date: t.date,
        })
        .collect();
    sort_newest_first(&mut releases);
    ReleaseRead {
        releases,
        error: None,
    }
}

/// Newest first; a tie (two tags on one commit) by tag, descending, so
/// `v1.0.1` sits over `v1.0.0`.
pub fn sort_newest_first(releases: &mut [Release]) {
    releases.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| b.tag.cmp(&a.tag)));
}

/// A tag and its creation date.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tag {
    name: String,
    date: DateTime<FixedOffset>,
}

/// What a repository's tag refs looked like when they were listed: the
/// count and newest modification time of the loose refs under `refs/tags/`
/// (directories included) and `packed-refs`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct TagStamp {
    loose: usize,
    newest: Option<SystemTime>,
    packed: Option<(SystemTime, u64)>,
}

/// Each repository's last tag listing and its stamp, by its git directory.
type TagCache = HashMap<PathBuf, (TagStamp, Vec<Tag>)>;
static CACHE: Mutex<Option<TagCache>> = Mutex::new(None);

/// Every tag of the repository at `repo`, from the cache while its tag refs
/// are unchanged.
fn repo_tags(repo: &Path) -> Result<Vec<Tag>, String> {
    let git_dir = git_common_dir(repo);
    let stamp = git_dir.as_deref().map(tag_stamp);
    if let (Some(dir), Some(stamp)) = (&git_dir, stamp) {
        let cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((cached, tags)) = cache.as_ref().and_then(|c| c.get(dir)) {
            if *cached == stamp {
                return Ok(tags.clone());
            }
        }
    }
    let tags = list_tags(repo)?;
    if let (Some(dir), Some(stamp)) = (git_dir, stamp) {
        let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        cache
            .get_or_insert_with(HashMap::new)
            .insert(dir, (stamp, tags.clone()));
    }
    Ok(tags)
}

/// `git for-each-ref refs/tags`: each tag's short name and creation date.
fn list_tags(repo: &Path) -> Result<Vec<Tag>, String> {
    let out = Command::new(git_binary())
        .arg("-C")
        .arg(repo)
        .args([
            "for-each-ref",
            "--format=%(refname:strip=2)%00%(creatordate:iso-strict)",
            "refs/tags",
        ])
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("couldn't run git to list the tags in {}: {e}", repo.display()))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("couldn't list the tags in {}: {why}", repo.display()));
    }
    Ok(parse_tags(&String::from_utf8_lossy(&out.stdout)))
}

/// `for-each-ref`'s lines, `<name>\0<date>`. A tag with no date (one on a
/// tree or a blob) is left out.
fn parse_tags(text: &str) -> Vec<Tag> {
    text.lines()
        .filter_map(|line| {
            let (name, date) = line.split_once('\0')?;
            let date = DateTime::parse_from_rfc3339(date.trim()).ok()?;
            Some(Tag {
                name: name.to_string(),
                date,
            })
        })
        .collect()
}

/// `git` on this process's `PATH`, else the login shell's (an app opened
/// from the Finder has a short `PATH`), else bare `git`.
fn git_binary() -> PathBuf {
    let here: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    crate::tools::find_on_path("git", &here)
        .or_else(|| crate::tools::find_on_path("git", &crate::tools::login_path_dirs()))
        .unwrap_or_else(|| PathBuf::from("git"))
}

/// The directory holding the repository at `repo`'s refs: `.git`, or for a
/// worktree (a `.git` file) its common directory.
pub(crate) fn git_common_dir(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = fs::read_to_string(&dot_git).ok()?;
    let dir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    let dir = if dir.is_relative() { repo.join(dir) } else { dir };
    Some(match fs::read_to_string(dir.join("commondir")) {
        Ok(common) => {
            let common = PathBuf::from(common.trim());
            if common.is_relative() {
                dir.join(common)
            } else {
                common
            }
        }
        Err(_) => dir,
    })
}

fn tag_stamp(git_dir: &Path) -> TagStamp {
    let mut stamp = TagStamp {
        packed: fs::metadata(git_dir.join("packed-refs"))
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len()))),
        ..TagStamp::default()
    };
    let mut dirs = vec![git_dir.join("refs").join("tags")];
    while let Some(dir) = dirs.pop() {
        let Ok(meta) = fs::metadata(&dir) else {
            continue;
        };
        stamp.newest = stamp.newest.max(meta.modified().ok());
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                dirs.push(entry.path());
            } else {
                stamp.loose += 1;
                stamp.newest = stamp.newest.max(meta.modified().ok());
            }
        }
    }
    stamp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str], date: &str) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn temp_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-releases-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"], "2026-11-01T00:00:00Z");
        dir
    }

    fn commit(repo: &Path, msg: &str, date: &str) {
        git(repo, &["commit", "-q", "--allow-empty", "-m", msg], date);
    }

    fn config(tags: &str, label: Option<&str>) -> ReleasesConfig {
        ReleasesConfig {
            tags: tags.into(),
            label: label.map(str::to_string),
        }
    }

    #[test]
    fn tags_are_dated_by_tagger_or_commit_and_sorted_newest_first() {
        let repo = temp_repo("dates");
        commit(&repo, "one", "2026-11-10T09:00:00-08:00");
        // Lightweight: the commit's date.
        git(&repo, &["tag", "v0.13.3"], "2026-11-16T12:00:00-08:00");
        commit(&repo, "two", "2026-11-18T09:00:00-08:00");
        // Annotated: the tagger's date (GIT_COMMITTER_DATE when tagging), not
        // the commit's.
        git(&repo, &["tag", "-a", "-m", "rel", "v0.14.0"], "2026-11-19T15:30:00-08:00");
        // Neither a release under this glob.
        git(&repo, &["tag", "nightly"], "2026-11-20T00:00:00Z");

        let read = read_releases_in(&config("v*.*.*", None), &repo);
        assert_eq!(read.error, None);
        let got: Vec<(&str, &str, String)> = read
            .releases
            .iter()
            .map(|r| (r.tag.as_str(), r.label.as_str(), r.date.to_rfc3339()))
            .collect();
        assert_eq!(
            got,
            [
                ("v0.14.0", "v0.14.0", "2026-11-19T15:30:00-08:00".to_string()),
                ("v0.13.3", "v0.13.3", "2026-11-10T09:00:00-08:00".to_string()),
            ]
        );
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn testflight_tags_label_by_version_and_a_new_tag_shows_through_the_cache() {
        let repo = temp_repo("testflight");
        commit(&repo, "one", "2026-11-10T09:00:00Z");
        git(&repo, &["tag", "testflight/1.2.0-1"], "2026-11-10T09:00:00Z");
        let cfg = config("testflight/*", Some("TestFlight {version}"));
        let labels = |read: ReleaseRead| -> Vec<String> {
            assert_eq!(read.error, None);
            read.releases.into_iter().map(|r| r.label).collect()
        };
        assert_eq!(labels(read_releases_in(&cfg, &repo)), ["TestFlight 1.2.0-1"]);
        // Read again: the same, from the cache.
        assert_eq!(labels(read_releases_in(&cfg, &repo)), ["TestFlight 1.2.0-1"]);
        commit(&repo, "two", "2026-11-12T09:00:00Z");
        git(&repo, &["tag", "testflight/1.2.0-2"], "2026-11-12T09:00:00Z");
        assert_eq!(
            labels(read_releases_in(&cfg, &repo)),
            ["TestFlight 1.2.0-2", "TestFlight 1.2.0-1"]
        );
        // Packed refs count too.
        git(&repo, &["pack-refs", "--all"], "2026-11-12T09:00:00Z");
        git(&repo, &["tag", "-d", "testflight/1.2.0-1"], "2026-11-12T09:00:00Z");
        assert_eq!(labels(read_releases_in(&cfg, &repo)), ["TestFlight 1.2.0-2"]);
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_bad_glob_or_a_missing_repo_is_an_error_not_a_panic() {
        let read = read_releases_in(&config("v[0-9", None), Path::new("/nonexistent"));
        assert!(read.releases.is_empty());
        let err = read.error.unwrap();
        assert!(err.starts_with("releases.tags `v[0-9`: "), "{err}");

        let missing = std::env::temp_dir().join("shelbi-releases-no-such-repo");
        let read = read_releases_in(&config("v*", None), &missing);
        assert!(read.releases.is_empty());
        assert!(read.error.unwrap().contains("couldn't"));
    }

    #[test]
    fn no_block_means_no_releases() {
        let p: Project = Project::from_yaml_str(
            "repo: /nonexistent\nmachines: []\norchestrator: { runner: claude }\nagent_runners: {}\n",
        )
        .unwrap();
        assert_eq!(read_releases_of(&p), ReleaseRead::default());
    }

    #[test]
    fn a_repo_url_reads_the_hub_machines_checkout() {
        let checkout = temp_repo("checkout");
        commit(&checkout, "one", "2026-11-10T09:00:00Z");
        git(&checkout, &["tag", "v1.0.0"], "2026-11-10T09:00:00Z");
        let yaml = format!(
            "repo: git@github.com:a/b.git\nmachines:\n  - {{ name: far, kind: ssh, host: far, work_dir: /nowhere }}\n  - {{ name: hub, kind: local, work_dir: {} }}\norchestrator: {{ runner: claude }}\nagent_runners: {{}}\nreleases:\n  tags: \"v*\"\n",
            checkout.display()
        );
        let p = Project::from_yaml_str(&yaml).unwrap();
        assert_eq!(local_checkout(&p), checkout);
        let read = read_releases_of(&p);
        assert_eq!(read.error, None);
        assert_eq!(read.releases.len(), 1);
        let _ = fs::remove_dir_all(&checkout);
    }

    #[test]
    fn for_each_ref_lines_parse_and_dateless_tags_drop() {
        let tags = parse_tags("v1\u{0}2026-11-19T15:30:00-08:00\ntree-tag\u{0}\n\nv2\u{0}2026-11-20T00:00:00Z\n");
        let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["v1", "v2"]);
    }

    #[test]
    fn ties_sort_by_tag_descending() {
        let date = DateTime::parse_from_rfc3339("2026-11-19T00:00:00Z").unwrap();
        let rel = |tag: &str| Release {
            tag: tag.into(),
            label: tag.into(),
            date,
        };
        let mut all = vec![rel("v1.0.0"), rel("v1.0.1")];
        sort_newest_first(&mut all);
        assert_eq!(all[0].tag, "v1.0.1");
    }
}
