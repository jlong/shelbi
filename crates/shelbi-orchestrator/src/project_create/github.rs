//! The GitHub reads behind Connect to an Issue tracker's "Choose your GitHub
//! project" pickers ([`shelbi_app::github_setup`]), run through this
//! machine's GitHub CLI: the signed-in account and its organizations, an
//! owner's repositories, and the preflight a GitHub connection runs before
//! it's written ([`super::issues::github_preflight`]). A far project's run on
//! its daemon (`far::Request::Github`), so they use that machine's `gh` and
//! its sign-in, as the connection itself will.
//!
//! Every call goes through the [`GhRun`] seam so the parsing and the
//! failure classification are unit-tested without `gh`.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use shelbi_app::github_setup::{GithubAccount, GithubAnswer, GithubFailure, GithubRequest};
use shelbi_app::tracker_connect::{ImportPreview, PreviewIssue, PREVIEW_ISSUES};

use super::issues::{classify_api_failure, github_preflight, GhCommandResult, GhProbe, GithubPreflightError};

/// How long one `gh` call may take before it's stopped (a hung network
/// mustn't leave a picker loading forever).
const GH_DEADLINE: Duration = Duration::from_secs(45);

/// The most repositories the picker lists for one owner.
const REPO_LIMIT: &str = "1000";

/// Runs one `gh` command. `None`: `gh` couldn't be started (not installed).
pub trait GhRun {
    fn gh(&mut self, args: &[&str]) -> Option<GhCommandResult>;
}

/// The real `gh`, with prompts off and a deadline.
pub struct RealGh;

impl GhRun for RealGh {
    fn gh(&mut self, args: &[&str]) -> Option<GhCommandResult> {
        let mut child = Command::new("gh")
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        let mut out = child.stdout.take();
        let mut err = child.stderr.take();
        let reader = std::thread::spawn(move || {
            let mut stdout = String::new();
            if let Some(o) = out.as_mut() {
                let _ = o.read_to_string(&mut stdout);
            }
            stdout
        });
        let err_reader = std::thread::spawn(move || {
            let mut stderr = String::new();
            if let Some(e) = err.as_mut() {
                let _ = e.read_to_string(&mut stderr);
            }
            stderr
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() < GH_DEADLINE => std::thread::sleep(Duration::from_millis(25)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let stdout = reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        Some(match status {
            Some(status) => GhCommandResult { success: status.success(), stdout, stderr },
            None => GhCommandResult {
                success: false,
                stdout,
                stderr: format!("gh took longer than {}s and was stopped", GH_DEADLINE.as_secs()),
            },
        })
    }
}

/// Answer `request` with this machine's `gh`. Blocking (it runs `gh` and the
/// network): keep it off any UI thread.
pub fn answer(request: GithubRequest) -> GithubAnswer {
    answer_with(&mut RealGh, request)
}

/// Answer `request` with `gh`.
pub fn answer_with<R: GhRun + ?Sized>(gh: &mut R, request: GithubRequest) -> GithubAnswer {
    match request {
        GithubRequest::Account => GithubAnswer::Account(account(gh)),
        GithubRequest::Repos { owner } => {
            let result = repos(gh, &owner);
            GithubAnswer::Repos { owner, result }
        }
        GithubRequest::Preflight { repo } => {
            let result = github_preflight(repo.trim(), &mut Probe(gh)).map_err(|f| f.guidance());
            GithubAnswer::Preflight { repo, result }
        }
    }
}

/// The signed-in login and its organizations. An organizations read that
/// fails (a token without `read:org`) lists none rather than failing.
fn account<R: GhRun + ?Sized>(gh: &mut R) -> Result<GithubAccount, GithubFailure> {
    let Some(user) = gh.gh(&["api", "user", "--jq", ".login"]) else {
        return Err(GithubFailure::NotInstalled);
    };
    if !user.success {
        return Err(match classify_api_failure("", &user.stderr) {
            GithubPreflightError::NotAuthenticated => GithubFailure::NotSignedIn,
            other => GithubFailure::Other(other.guidance()),
        });
    }
    let login = user.stdout.trim().to_string();
    if login.is_empty() {
        return Err(GithubFailure::Other("gh returned no login for the signed-in account".into()));
    }
    let orgs = gh
        .gh(&["api", "user/orgs", "--paginate", "--jq", ".[].login"])
        .filter(|r| r.success)
        .map(|r| lines(&r.stdout))
        .unwrap_or_default();
    Ok(GithubAccount { login, orgs })
}

/// `owner`'s unarchived repositories the account can see, as `owner/name`.
fn repos<R: GhRun + ?Sized>(gh: &mut R, owner: &str) -> Result<Vec<String>, String> {
    let args = [
        "repo",
        "list",
        owner,
        "--limit",
        REPO_LIMIT,
        "--no-archived",
        "--json",
        "nameWithOwner",
        "--jq",
        ".[].nameWithOwner",
    ];
    let Some(out) = gh.gh(&args) else {
        return Err(GithubPreflightError::GhNotInstalled.guidance());
    };
    if !out.success {
        return Err(match classify_api_failure("", &out.stderr) {
            GithubPreflightError::ProbeFailed { detail } => format!("Couldn't list {owner}'s repositories: {detail}"),
            other => other.guidance(),
        });
    }
    Ok(lines(&out.stdout))
}

/// The Review your first import read for `repo`: its open and closed issue
/// counts (pull requests aside), how many open ones are the signed-in
/// account's or no one's, and the newest few, in one GraphQL call.
pub fn import_preview(repo: &str) -> Result<ImportPreview, String> {
    import_preview_with(&mut RealGh, repo)
}

const PREVIEW_QUERY: &str = "query($owner:String!,$name:String!,$mine:String!,$none:String!){\
repository(owner:$owner,name:$name){open:issues(states:OPEN){totalCount} \
closed:issues(states:CLOSED){totalCount} \
recent:issues(states:OPEN,first:3,orderBy:{field:CREATED_AT,direction:DESC}){nodes{number title}}} \
mine:search(type:ISSUE,query:$mine){issueCount} unassigned:search(type:ISSUE,query:$none){issueCount}}";

/// [`import_preview`] over `gh`.
pub fn import_preview_with<R: GhRun + ?Sized>(gh: &mut R, repo: &str) -> Result<ImportPreview, String> {
    let repo = repo.trim();
    let Some((owner, name)) = repo.split_once('/').filter(|(o, n)| !o.is_empty() && !n.is_empty()) else {
        return Err(format!("`{repo}` isn't an owner/repo"));
    };
    let query = format!("query={PREVIEW_QUERY}");
    let (owner, name) = (format!("owner={owner}"), format!("name={name}"));
    let mine = format!("mine=repo:{repo} is:issue is:open assignee:@me");
    let none = format!("none=repo:{repo} is:issue is:open no:assignee");
    let args = ["api", "graphql", "-f", &query, "-f", &owner, "-f", &name, "-f", &mine, "-f", &none];
    let Some(out) = gh.gh(&args) else {
        return Err(GithubPreflightError::GhNotInstalled.guidance());
    };
    if !out.success {
        return Err(match classify_api_failure(repo, &out.stderr) {
            GithubPreflightError::ProbeFailed { detail } => format!("Couldn't read {repo}'s issues: {detail}"),
            other => other.guidance(),
        });
    }
    parse_preview(repo, &out.stdout)
}

fn parse_preview(repo: &str, stdout: &str) -> Result<ImportPreview, String> {
    let bad = |why: &str| format!("Couldn't read {repo}'s issues: {why}");
    let v: serde_json::Value = serde_json::from_str(stdout).map_err(|e| bad(&e.to_string()))?;
    if let Some(msg) = v.pointer("/errors/0/message").and_then(|m| m.as_str()) {
        return Err(bad(msg));
    }
    let count = |path: &str| v.pointer(path).and_then(|n| n.as_u64()).map(|n| n as usize);
    let open = count("/data/repository/open/totalCount").ok_or_else(|| bad("GitHub returned no repository"))?;
    let first = v
        .pointer("/data/repository/recent/nodes")
        .and_then(|n| n.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| {
                    let number = n.get("number")?.as_u64()?;
                    let title = n.get("title")?.as_str()?.to_string();
                    Some(PreviewIssue { key: format!("#{number}"), title })
                })
                .take(PREVIEW_ISSUES)
                .collect()
        })
        .unwrap_or_default();
    Ok(ImportPreview {
        open,
        mine: count("/data/mine/issueCount").unwrap_or(0).min(open),
        unassigned: count("/data/unassigned/issueCount").unwrap_or(0).min(open),
        closed: count("/data/repository/closed/totalCount").unwrap_or(0),
        closed_capped: false,
        first,
    })
}

/// One value per non-blank line.
fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// The preflight's probe over a [`GhRun`].
struct Probe<'a, R: GhRun + ?Sized>(&'a mut R);

impl<R: GhRun + ?Sized> GhProbe for Probe<'_, R> {
    fn gh_version(&mut self) -> Option<String> {
        self.0.gh(&["--version"]).filter(|r| r.success).map(|r| r.stdout.trim().to_string())
    }

    fn gh_api_repo(&mut self, repo: &str) -> GhCommandResult {
        let path = format!("repos/{repo}");
        self.0.gh(&["api", &path]).unwrap_or_else(|| GhCommandResult {
            success: false,
            stdout: String::new(),
            stderr: "could not run gh".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canned `gh`: answers by the command's first two words; records each.
    #[derive(Default)]
    struct FakeGh {
        installed: bool,
        signed_in: bool,
        orgs_fail: bool,
        asked: Vec<Vec<String>>,
    }

    fn ok(stdout: &str) -> Option<GhCommandResult> {
        Some(GhCommandResult { success: true, stdout: stdout.into(), stderr: String::new() })
    }

    fn failed(stderr: &str) -> Option<GhCommandResult> {
        Some(GhCommandResult { success: false, stdout: String::new(), stderr: stderr.into() })
    }

    impl GhRun for FakeGh {
        fn gh(&mut self, args: &[&str]) -> Option<GhCommandResult> {
            self.asked.push(args.iter().map(|a| a.to_string()).collect());
            if !self.installed {
                return None;
            }
            if !self.signed_in && args != ["--version"] {
                return failed("To get started with GitHub CLI, please run:  gh auth login");
            }
            match args {
                ["--version"] => ok("gh version 2.60.0"),
                ["api", "user", ..] => ok("octocat\n"),
                ["api", "user/orgs", ..] if self.orgs_fail => failed("HTTP 403: missing read:org"),
                ["api", "user/orgs", ..] => ok("acme\nwidgets\n"),
                ["repo", "list", "acme", ..] => ok("acme/app\nacme/site\n\n"),
                ["repo", "list", "nobody", ..] => failed("GraphQL: Could not resolve to a RepositoryOwner"),
                ["api", "graphql", ..] => ok(PREVIEW_JSON),
                ["api", path] if path.starts_with("repos/") => {
                    ok(r#"{"permissions":{"push":true},"has_issues":true}"#)
                }
                other => panic!("unexpected gh {other:?}"),
            }
        }
    }

    const PREVIEW_JSON: &str = r#"{"data":{"repository":{"open":{"totalCount":64},"closed":{"totalCount":12},
        "recent":{"nodes":[{"number":130,"title":"Keep the sidebar width"},{"number":129,"title":"Restore the previous view"}]}},
        "mine":{"issueCount":48},"unassigned":{"issueCount":4}}}"#;

    #[test]
    fn the_import_preview_is_one_graphql_read() {
        let mut gh = signed_in();
        let preview = import_preview_with(&mut gh, " acme/app ").unwrap();
        assert_eq!((preview.open, preview.mine, preview.unassigned, preview.closed), (64, 48, 4, 12));
        assert_eq!(preview.others(), 12);
        assert_eq!(
            preview.first,
            vec![
                PreviewIssue { key: "#130".into(), title: "Keep the sidebar width".into() },
                PreviewIssue { key: "#129".into(), title: "Restore the previous view".into() },
            ]
        );
        let args = &gh.asked[0];
        assert_eq!(&args[..2], ["api", "graphql"]);
        assert!(args.contains(&"owner=acme".to_string()) && args.contains(&"name=app".to_string()), "{args:?}");
        assert!(args.contains(&"mine=repo:acme/app is:issue is:open assignee:@me".to_string()), "{args:?}");
        assert!(args.iter().any(|a| a.starts_with("query=query(") && !a.contains('\\')), "{args:?}");

        let mut gh = FakeGh { installed: true, ..FakeGh::default() };
        assert!(import_preview_with(&mut gh, "acme/app").unwrap_err().contains("gh auth login"));
        assert!(import_preview_with(&mut signed_in(), "acme").is_err(), "not owner/repo");
        let err = parse_preview("acme/app", r#"{"errors":[{"message":"Could not resolve to a Repository"}]}"#);
        assert_eq!(err.unwrap_err(), "Couldn't read acme/app's issues: Could not resolve to a Repository");
    }

    fn signed_in() -> FakeGh {
        FakeGh { installed: true, signed_in: true, ..FakeGh::default() }
    }

    #[test]
    fn the_account_is_the_login_and_its_organizations() {
        let mut gh = signed_in();
        let answer = answer_with(&mut gh, GithubRequest::Account);
        assert_eq!(
            answer,
            GithubAnswer::Account(Ok(GithubAccount { login: "octocat".into(), orgs: vec!["acme".into(), "widgets".into()] }))
        );
        assert_eq!(gh.asked[0], ["api", "user", "--jq", ".login"]);

        let mut gh = FakeGh { orgs_fail: true, ..signed_in() };
        let GithubAnswer::Account(Ok(account)) = answer_with(&mut gh, GithubRequest::Account) else { panic!() };
        assert!(account.orgs.is_empty(), "no read:org lists no organizations rather than failing");
    }

    #[test]
    fn signed_out_or_missing_gh_says_which() {
        let mut gh = FakeGh { installed: true, ..FakeGh::default() };
        assert_eq!(answer_with(&mut gh, GithubRequest::Account), GithubAnswer::Account(Err(GithubFailure::NotSignedIn)));
        let mut gh = FakeGh::default();
        assert_eq!(answer_with(&mut gh, GithubRequest::Account), GithubAnswer::Account(Err(GithubFailure::NotInstalled)));
    }

    #[test]
    fn an_owners_repositories_list_one_per_line() {
        let mut gh = signed_in();
        let answer = answer_with(&mut gh, GithubRequest::Repos { owner: "acme".into() });
        assert_eq!(
            answer,
            GithubAnswer::Repos { owner: "acme".into(), result: Ok(vec!["acme/app".into(), "acme/site".into()]) }
        );
        assert!(gh.asked[0].iter().any(|a| a == "--no-archived"));
        let GithubAnswer::Repos { result: Err(why), .. } = answer_with(&mut gh, GithubRequest::Repos { owner: "nobody".into() })
        else {
            panic!()
        };
        assert!(why.contains("nobody") && why.contains("RepositoryOwner"), "{why}");
    }

    #[test]
    fn the_preflight_is_the_connections() {
        let mut gh = signed_in();
        assert_eq!(
            answer_with(&mut gh, GithubRequest::Preflight { repo: "acme/app".into() }),
            GithubAnswer::Preflight { repo: "acme/app".into(), result: Ok(()) }
        );
        assert_eq!(gh.asked.last().unwrap(), &["api", "repos/acme/app"]);
        let mut gh = FakeGh { installed: true, ..FakeGh::default() };
        let GithubAnswer::Preflight { result: Err(why), .. } =
            answer_with(&mut gh, GithubRequest::Preflight { repo: "acme/app".into() })
        else {
            panic!()
        };
        assert!(why.contains("gh auth login"), "{why}");
    }
}
