//! The GitHub half of Connect to an Issue tracker's "Choose your GitHub
//! project" step (`tc-choose`, Figma 2753:12004): the account the GitHub CLI
//! is signed in to, the owner (the account or one of its organizations), and
//! one of that owner's repositories, each read through `gh` where the project
//! lives.
//!
//! Like [`crate::linear_setup`], [`GithubChoice`] never does IO. It says
//! which read is due ([`GithubChoice::due`], one [`GithubRequest`] at a
//! time; a host loops until it answers `None`), the host runs it where the
//! project lives (`shelbi_orchestrator::project_create::github::answer`, or
//! the far daemon's `AppRequest` for a project on another machine) off the
//! UI thread, and hands the [`GithubAnswer`] back ([`GithubChoice::answer`]).
//! An answer for an owner or a repository the user has since changed is
//! dropped.
//!
//! The order a host sees:
//!
//! 1. [`GithubRequest::Account`] as soon as GitHub is chosen: the signed-in
//!    login and its organizations. It also proves `gh` works: a missing `gh`
//!    or a signed-out one turns into [`GithubAuth::Missing`], and the view
//!    shows how to sign in ([`AUTH_COMMAND`]) with a Retry.
//! 2. [`GithubRequest::Repos`] for the picked owner (the login, until
//!    another is picked).
//! 3. [`GithubRequest::Preflight`] when Continue is pressed
//!    ([`GithubChoice::want_preflight`]): the check a GitHub connection runs
//!    before it's written (`gh` signed in, the repository visible, Issues on,
//!    push access). Continue moves on once the repository as picked now has
//!    passed ([`GithubChoice::ready`]).

use serde::{Deserialize, Serialize};

use crate::linear_setup::Loadable;
use crate::new_project::github_config;

/// The pickers' labels.
pub const OWNER_LABEL: &str = "Organization";
pub const REPO_LABEL: &str = "Repository";
/// The repository picker before one is picked, and with none to pick.
pub const CHOOSE_REPO: &str = "Choose a repository";
pub const NO_REPOS: &str = "No repositories";
/// Continue was pressed with no repository.
pub const REPO_REQUIRED: &str = "Choose the repository whose issues you want to work on.";
/// The command that signs the GitHub CLI in.
pub const AUTH_COMMAND: &str = "gh auth login";
/// What shows while `gh` isn't signed in, or isn't installed.
pub const NOT_SIGNED_IN: &str = "Shelbi reads GitHub through the GitHub CLI (gh) on this project's machine, \
     and it isn't signed in. Run this in a terminal there, then Retry:";
pub const NOT_INSTALLED: &str = "Shelbi reads GitHub through the GitHub CLI (gh), which isn't installed on \
     this project's machine. Install it from https://cli.github.com, sign in, then Retry:";
/// Change account's explanation: Shelbi uses whichever account `gh` is
/// signed in to.
pub const CHANGE_ACCOUNT_HELP: &str = "Shelbi uses the account the GitHub CLI (gh) is signed in to on this \
     project's machine. To use another, sign in to it (or switch with `gh auth switch`) in a terminal \
     there, then Retry:";

/// A read the host runs for a [`GithubChoice`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubRequest {
    /// The signed-in login and its organizations (and so, whether `gh`
    /// works).
    Account,
    /// The repositories `owner` has that the account can see.
    Repos { owner: String },
    /// The full check before a GitHub connection is written
    /// (`project_create::issues::preflight_github`).
    Preflight { repo: String },
}

/// Why the account read failed, as far as signing in is concerned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubFailure {
    /// `gh` isn't on the machine's PATH.
    NotInstalled,
    /// `gh` isn't signed in (or its token was refused).
    NotSignedIn,
    /// Anything else (network, rate limit, a GitHub error).
    Other(String),
}

/// The signed-in account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubAccount {
    pub login: String,
    /// The organizations it belongs to (their logins).
    #[serde(default)]
    pub orgs: Vec<String>,
}

/// The host's answer to a [`GithubRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubAnswer {
    Account(Result<GithubAccount, GithubFailure>),
    /// `owner`'s repositories as `owner/name`.
    Repos { owner: String, result: Result<Vec<String>, String> },
    /// `repo` passed, or failed with the fix to show.
    Preflight { repo: String, result: Result<(), String> },
}

/// What's known about the GitHub CLI's sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GithubAuth {
    /// Not checked yet (the account read checks it).
    #[default]
    Unknown,
    /// `gh` can't read GitHub: not installed (`false`) or not signed in.
    Missing { installed: bool },
    /// Signed in.
    Valid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Preflight {
    Wanted,
    Running(String),
}

/// The GitHub choice: the account, the owner and repository as picked, the
/// lists the pickers show, and whether the repository passed the preflight.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GithubChoice {
    auth: GithubAuth,
    account: Loadable<GithubAccount>,
    /// Change account opened its explanation over a working sign-in.
    changing_account: bool,
    owner: String,
    /// One owner's repositories.
    repos: Option<(String, Loadable<Vec<String>>)>,
    repo: String,
    preflight: Option<Preflight>,
    /// The repository that last passed the preflight.
    verified: Option<String>,
    /// Why Continue was refused, or the last preflight failed.
    error: Option<String>,
}

impl GithubChoice {
    /// A choice seeded with a repository (`owner/name`): its owner is picked.
    pub fn with_repo(repo: &str) -> Self {
        let repo = repo.trim().to_string();
        let owner = repo.split_once('/').map(|(o, _)| o.to_string()).unwrap_or_default();
        GithubChoice { owner, repo, ..GithubChoice::default() }
    }

    pub fn auth(&self) -> &GithubAuth {
        &self.auth
    }

    /// The sign-in instructions show: `gh` can't read GitHub, or Change
    /// account opened them.
    pub fn needs_auth(&self) -> bool {
        self.changing_account || matches!(self.auth, GithubAuth::Missing { .. })
    }

    /// Change account opened the instructions over a working sign-in.
    pub fn changing_account(&self) -> bool {
        self.changing_account
    }

    /// The instructions' text (the command, [`AUTH_COMMAND`], goes under
    /// it), while [`Self::needs_auth`].
    pub fn auth_message(&self) -> Option<&'static str> {
        match self.auth {
            GithubAuth::Missing { installed: false } => Some(NOT_INSTALLED),
            GithubAuth::Missing { installed: true } => Some(NOT_SIGNED_IN),
            _ if self.changing_account => Some(CHANGE_ACCOUNT_HELP),
            _ => None,
        }
    }

    /// Change account: explain how to sign `gh` in to another account. The
    /// current one stays in use until Retry reads the account again.
    pub fn change_account(&mut self) {
        if self.auth == GithubAuth::Valid {
            self.changing_account = true;
            self.error = None;
        }
    }

    /// Close Change account's explanation, keeping the account.
    pub fn cancel_change_account(&mut self) {
        self.changing_account = false;
    }

    /// The signed-in login, once read.
    pub fn login(&self) -> Option<&str> {
        self.account.loaded().map(|a| a.login.as_str())
    }

    pub fn account(&self) -> &Loadable<GithubAccount> {
        &self.account
    }

    /// The organization picker's rows: the login first, then its
    /// organizations. `None` until the account is read.
    pub fn owners(&self) -> Option<Vec<String>> {
        let account = self.account.loaded()?;
        let mut rows = vec![account.login.clone()];
        rows.extend(account.orgs.iter().filter(|o| !o.eq_ignore_ascii_case(&account.login)).cloned());
        Some(rows)
    }

    /// The owner as picked (empty until the account is read).
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The repository picker's rows: the picked owner's repositories.
    /// [`Loadable::Idle`] until an owner is picked.
    pub fn repos(&self) -> Loadable<Vec<String>> {
        match &self.repos {
            Some((owner, list)) if *owner == self.owner && !self.owner.is_empty() => list.clone(),
            _ => Loadable::Idle,
        }
    }

    /// The repository as picked (`owner/name`); empty until one is.
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Why Continue was refused, or why the repository didn't pass.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The preflight is due or running.
    pub fn preflighting(&self) -> bool {
        self.preflight.is_some()
    }

    /// The repository as picked now passed the preflight.
    pub fn ready(&self) -> bool {
        !self.preflighting() && self.verified.as_deref().is_some_and(|r| r == self.repo.trim()) && !self.repo.is_empty()
    }

    /// Pick the owner. A repository of another owner is dropped (the
    /// repository picker lists only this one's).
    pub fn select_owner(&mut self, owner: &str) {
        let owner = owner.trim();
        if owner == self.owner {
            return;
        }
        self.owner = owner.to_string();
        if !owner_of(&self.repo).eq_ignore_ascii_case(owner) {
            self.repo.clear();
        }
        self.error = None;
    }

    /// Pick the repository (`owner/name`). A typed one names its owner too.
    pub fn select_repo(&mut self, repo: &str) {
        let repo = repo.trim();
        if repo == self.repo {
            return;
        }
        self.repo = repo.to_string();
        let owner = owner_of(repo);
        if !owner.is_empty() && !owner.eq_ignore_ascii_case(&self.owner) {
            self.owner = owner.to_string();
        }
        self.error = None;
    }

    /// Continue: run the preflight on the repository as picked. Refuses a
    /// missing or malformed one. False when nothing was started.
    pub fn want_preflight(&mut self) -> bool {
        if self.preflighting() || self.needs_auth() {
            return false;
        }
        if self.repo.trim().is_empty() {
            self.error = Some(REPO_REQUIRED.into());
            return false;
        }
        if let Err(why) = github_config(&self.repo) {
            self.error = Some(why);
            return false;
        }
        self.error = None;
        self.preflight = Some(Preflight::Wanted);
        true
    }

    /// Retry: read the account again (after a sign-in, or to pick up
    /// another account), and every list whose read failed.
    pub fn retry(&mut self) {
        if self.needs_auth() || self.account.error().is_some() {
            self.account = Loadable::Idle;
            self.auth = GithubAuth::Unknown;
            self.changing_account = false;
            // Another account sees other repositories.
            self.repos = None;
            self.verified = None;
            self.error = None;
        }
        if matches!(&self.repos, Some((_, Loadable::Failed(_)))) {
            self.repos = None;
        }
    }

    /// The next read the host should run, marked as running. `None` when
    /// nothing is due.
    pub fn due(&mut self) -> Option<GithubRequest> {
        if self.account == Loadable::Idle && self.auth == GithubAuth::Unknown {
            self.account = Loadable::Loading;
            return Some(GithubRequest::Account);
        }
        if self.auth != GithubAuth::Valid {
            return None;
        }
        if !self.owner.is_empty() && self.repos.as_ref().is_none_or(|(o, _)| *o != self.owner) {
            self.repos = Some((self.owner.clone(), Loadable::Loading));
            return Some(GithubRequest::Repos { owner: self.owner.clone() });
        }
        if self.preflight == Some(Preflight::Wanted) {
            let repo = self.repo.trim().to_string();
            self.preflight = Some(Preflight::Running(repo.clone()));
            return Some(GithubRequest::Preflight { repo });
        }
        None
    }

    /// The host's answer to a request [`Self::due`] gave it.
    pub fn answer(&mut self, answer: GithubAnswer) {
        match answer {
            GithubAnswer::Account(result) => {
                if !self.account.is_loading() {
                    return;
                }
                match result {
                    Ok(account) => {
                        self.auth = GithubAuth::Valid;
                        let owners = {
                            let mut o = vec![account.login.clone()];
                            o.extend(account.orgs.iter().cloned());
                            o
                        };
                        // The login until another owner is picked; a kept
                        // owner the new account can't see falls back to it.
                        if !owners.iter().any(|o| o.eq_ignore_ascii_case(&self.owner)) {
                            let login = account.login.clone();
                            self.owner.clear();
                            self.select_owner(&login);
                        }
                        self.account = Loadable::Loaded(account);
                    }
                    Err(GithubFailure::NotInstalled) => {
                        self.auth = GithubAuth::Missing { installed: false };
                        self.account = Loadable::Idle;
                    }
                    Err(GithubFailure::NotSignedIn) => {
                        self.auth = GithubAuth::Missing { installed: true };
                        self.account = Loadable::Idle;
                    }
                    Err(GithubFailure::Other(why)) => {
                        // `gh` works but the read didn't: say so, with Retry.
                        self.auth = GithubAuth::Valid;
                        self.account = Loadable::Failed(why);
                    }
                }
            }
            GithubAnswer::Repos { owner, result } => {
                if let Some((_, list)) = self.repos.as_mut().filter(|(o, _)| *o == owner) {
                    *list = match result {
                        Ok(mut repos) => {
                            repos.sort_by_key(|r| r.to_lowercase());
                            Loadable::Loaded(repos)
                        }
                        Err(why) => Loadable::Failed(why),
                    };
                }
            }
            GithubAnswer::Preflight { repo, result } => {
                if !matches!(&self.preflight, Some(Preflight::Running(r)) if *r == repo) {
                    return;
                }
                self.preflight = None;
                if repo != self.repo.trim() {
                    // Changed while it ran: Continue checks the new one.
                    return;
                }
                match result {
                    Ok(()) => {
                        self.verified = Some(repo);
                        self.error = None;
                    }
                    Err(why) => {
                        self.verified = None;
                        self.error = Some(why);
                    }
                }
            }
        }
    }
}

/// The owner part of `owner/name` (empty without a slash).
fn owner_of(repo: &str) -> &str {
    repo.split_once('/').map(|(o, _)| o.trim()).unwrap_or("")
}

/// Canned GitHub answers for the clients' tests and the desktop's visual
/// check: `octocat`, a member of `acme`, with two repositories each. Nothing
/// here runs `gh`.
#[doc(hidden)]
pub mod fixture {
    use super::*;

    pub const LOGIN: &str = "octocat";
    pub const ORG: &str = "acme";

    /// The answer to `request` with `gh` signed in.
    pub fn answer(request: GithubRequest) -> GithubAnswer {
        match request {
            GithubRequest::Account => {
                GithubAnswer::Account(Ok(GithubAccount { login: LOGIN.into(), orgs: vec![ORG.into()] }))
            }
            GithubRequest::Repos { owner } => {
                let result = Ok(match owner.as_str() {
                    LOGIN => vec!["octocat/hello-world".into(), "octocat/shelbi".into()],
                    ORG => vec!["acme/site".into(), "acme/app".into()],
                    _ => Vec::new(),
                });
                GithubAnswer::Repos { owner, result }
            }
            GithubRequest::Preflight { repo } => GithubAnswer::Preflight { repo, result: Ok(()) },
        }
    }

    /// The answer to `request` on a machine whose `gh` isn't signed in.
    pub fn signed_out(request: GithubRequest) -> GithubAnswer {
        match request {
            GithubRequest::Account => GithubAnswer::Account(Err(GithubFailure::NotSignedIn)),
            other => answer(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run every due read with `answer`.
    fn settle(c: &mut GithubChoice, answer: fn(GithubRequest) -> GithubAnswer) {
        while let Some(req) = c.due() {
            c.answer(answer(req));
        }
    }

    #[test]
    fn the_account_loads_first_then_the_logins_repositories() {
        let mut c = GithubChoice::default();
        assert_eq!(c.due(), Some(GithubRequest::Account));
        assert!(c.account().is_loading());
        assert_eq!(c.due(), None, "one account read at a time");
        assert_eq!(c.owners(), None);

        c.answer(fixture::answer(GithubRequest::Account));
        assert_eq!(c.auth(), &GithubAuth::Valid);
        assert_eq!(c.owners(), Some(vec!["octocat".to_string(), "acme".to_string()]));
        assert_eq!(c.owner(), "octocat", "the login until another is picked");
        assert_eq!(c.due(), Some(GithubRequest::Repos { owner: "octocat".into() }));
        assert!(c.repos().is_loading());
        c.answer(fixture::answer(GithubRequest::Repos { owner: "octocat".into() }));
        assert_eq!(c.repos().loaded().unwrap(), &vec!["octocat/hello-world".to_string(), "octocat/shelbi".into()]);
        assert_eq!(c.due(), None);
    }

    #[test]
    fn the_repositories_follow_the_organization() {
        let mut c = GithubChoice::default();
        settle(&mut c, fixture::answer);
        c.select_repo("octocat/shelbi");
        c.select_owner("acme");
        assert_eq!(c.repo(), "", "a repository of another owner is dropped");
        assert_eq!(c.repos(), Loadable::Idle, "octocat's list isn't acme's");
        assert_eq!(c.due(), Some(GithubRequest::Repos { owner: "acme".into() }));
        // A late answer for the old owner is dropped.
        c.answer(fixture::answer(GithubRequest::Repos { owner: "octocat".into() }));
        assert!(c.repos().is_loading());
        c.answer(fixture::answer(GithubRequest::Repos { owner: "acme".into() }));
        assert_eq!(c.repos().loaded().unwrap(), &vec!["acme/app".to_string(), "acme/site".into()], "sorted");

        c.select_repo("acme/app");
        c.select_owner("acme");
        assert_eq!(c.repo(), "acme/app", "the same owner keeps it");
        c.select_repo("octocat/shelbi");
        assert_eq!(c.owner(), "octocat", "a typed repository names its owner");
    }

    #[test]
    fn signed_out_shows_how_to_sign_in_and_retry_reads_again() {
        let mut c = GithubChoice::default();
        settle(&mut c, fixture::signed_out);
        assert_eq!(c.auth(), &GithubAuth::Missing { installed: true });
        assert!(c.needs_auth());
        assert_eq!(c.auth_message(), Some(NOT_SIGNED_IN));
        assert_eq!(c.due(), None, "nothing more until it's signed in");
        assert!(!c.want_preflight());

        c.retry();
        assert_eq!(c.due(), Some(GithubRequest::Account));
        c.answer(fixture::answer(GithubRequest::Account));
        assert!(!c.needs_auth() && c.auth_message().is_none());

        let mut c = GithubChoice::default();
        c.due();
        c.answer(GithubAnswer::Account(Err(GithubFailure::NotInstalled)));
        assert_eq!(c.auth_message(), Some(NOT_INSTALLED));
    }

    #[test]
    fn change_account_explains_and_retry_picks_up_the_new_one() {
        let mut c = GithubChoice::default();
        settle(&mut c, fixture::answer);
        c.select_owner("acme");
        settle(&mut c, fixture::answer);
        c.select_repo("acme/app");
        c.change_account();
        assert!(c.needs_auth() && c.changing_account());
        assert_eq!(c.auth_message(), Some(CHANGE_ACCOUNT_HELP));
        c.cancel_change_account();
        assert!(!c.needs_auth());

        c.change_account();
        c.retry();
        assert_eq!(c.due(), Some(GithubRequest::Account));
        // The new account isn't in acme: back to its login, repo dropped.
        c.answer(GithubAnswer::Account(Ok(GithubAccount { login: "hubot".into(), orgs: vec![] })));
        assert_eq!((c.owner(), c.repo()), ("hubot", ""));
        assert!(!c.changing_account());
    }

    #[test]
    fn continue_needs_a_repository_then_a_passed_preflight_of_it() {
        let mut c = GithubChoice::default();
        settle(&mut c, fixture::answer);
        assert!(!c.want_preflight());
        assert_eq!(c.error(), Some(REPO_REQUIRED));
        c.select_repo("not a repo");
        assert!(!c.want_preflight());
        assert!(c.error().unwrap().contains("owner/repo"), "{:?}", c.error());

        c.select_repo("octocat/shelbi");
        assert_eq!(c.error(), None, "an edit clears it");
        assert!(c.want_preflight());
        assert_eq!(c.due(), Some(GithubRequest::Preflight { repo: "octocat/shelbi".into() }));
        assert!(!c.want_preflight(), "one at a time");
        c.answer(GithubAnswer::Preflight { repo: "octocat/shelbi".into(), result: Err("Issues disabled".into()) });
        assert!(!c.ready());
        assert_eq!(c.error(), Some("Issues disabled"));

        assert!(c.want_preflight());
        c.due();
        c.answer(GithubAnswer::Preflight { repo: "octocat/shelbi".into(), result: Ok(()) });
        assert!(c.ready());
        c.select_repo("octocat/hello-world");
        assert!(!c.ready(), "a changed pick is checked again");

        // An answer for a pick changed while it ran is dropped.
        assert!(c.want_preflight());
        c.due();
        c.select_repo("octocat/shelbi");
        c.answer(GithubAnswer::Preflight { repo: "octocat/hello-world".into(), result: Ok(()) });
        assert!(!c.preflighting());
        assert!(c.ready(), "octocat/shelbi passed before");
        c.select_repo("octocat/hello-world");
        assert!(!c.ready(), "the dropped pass wasn't recorded");
    }

    #[test]
    fn a_failed_read_says_so_and_retry_asks_again() {
        let mut c = GithubChoice::default();
        c.due();
        c.answer(GithubAnswer::Account(Err(GithubFailure::Other("rate limited".into()))));
        assert!(!c.needs_auth());
        assert_eq!(c.account().error(), Some("rate limited"));
        c.retry();
        assert_eq!(c.due(), Some(GithubRequest::Account));
        c.answer(fixture::answer(GithubRequest::Account));
        c.due();
        c.answer(GithubAnswer::Repos { owner: "octocat".into(), result: Err("timeout".into()) });
        assert_eq!(c.repos().error(), Some("timeout"));
        c.retry();
        assert_eq!(c.due(), Some(GithubRequest::Repos { owner: "octocat".into() }));
    }

    #[test]
    fn a_seeded_repository_picks_its_owner() {
        let mut c = GithubChoice::with_repo(" acme/app ");
        assert_eq!((c.owner(), c.repo()), ("acme", "acme/app"));
        settle(&mut c, fixture::answer);
        assert_eq!(c.owner(), "acme", "kept: the account is in acme");
    }
}
