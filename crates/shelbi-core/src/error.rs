use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// Discovery walked up from cwd, found an in-repo `.shelbi/project.yaml`
    /// naming project `name`, but the per-user companion `local.yaml`
    /// (which pins this user's machines and workspace pool) is absent.
    /// Fresh-clone case: the caller should prompt for
    /// `shelbi init --pick-up`. Distinct from [`Error::Yaml`] so callers
    /// can tell "config exists but not registered locally" apart from
    /// "config is broken".
    #[error(
        "project `{name}` is registered in-repo at {} but has not been picked up \
         on this machine (missing {}); run `shelbi init --pick-up` to register \
         your local machines and workspaces",
        config_path.display(),
        expected_local.display()
    )]
    ProjectNotPickedUp {
        name: String,
        config_path: PathBuf,
        expected_local: PathBuf,
    },

    #[error(
        "invalid agent id `{0}`: only lowercase ASCII letters, digits, `-`, and `_` \
         are allowed and it must start with a lowercase letter or digit (uppercase is \
         rejected so ids don't collide on case-insensitive filesystems)"
    )]
    InvalidAgentId(String),

    /// A project name isn't a single safe path component or isn't a valid
    /// agent id (empty, contains a path separator or `.`/`..`, or uses a
    /// character outside the agent-id charset). Rejected at the storage-layer
    /// chokepoint ([`crate::validate_project_name`]) so a `../`-style name
    /// can't escape `~/.shelbi/projects/` and a name that isn't a valid agent
    /// id can't crash dashboard setup. Onboarding normalizes names into this
    /// charset first (see [`crate::normalize_project_name`]).
    #[error(
        "invalid project name `{0}`: must be a single path component of \
         lowercase `[a-z0-9_-]` starting with a letter or digit (try something \
         like `my-app`)"
    )]
    InvalidProjectName(String),

    /// A task file's frontmatter `id:` doesn't match the filename it was
    /// loaded from. Left unchecked this forks the card into two files on the
    /// next write (read by filename, write by frontmatter id). Surfaces both
    /// values so the user can reconcile the hand-edited file.
    #[error(
        "task file `{requested}.md` declares mismatched frontmatter id \
         `{found}`; rename the file or fix the `id:` field so they match"
    )]
    IssueIdMismatch { requested: String, found: String },

    #[error(
        "task id `{id}` is too long: {len} bytes (max {max}); git ref names \
         must leave room for a generated branch prefix under GitHub's 255-byte ref limit"
    )]
    IssueIdTooLong { id: String, len: usize, max: usize },

    /// A task's `branch:` override carries characters outside the safe set
    /// (task-id charset plus `/`). Left unchecked the value reaches
    /// `git checkout` / `git worktree add` on a possibly-remote worker, where
    /// a leading `-` is parsed as a git flag (argument injection) and shell
    /// metacharacters would re-tokenize on the SSH wire. Rejected at the
    /// save chokepoint ([`crate::validate_branch`]).
    #[error(
        "invalid branch `{0}`: only ASCII letters, digits, `-`, `_`, and `/` are \
         allowed and it must start with a letter or digit"
    )]
    InvalidBranch(String),

    #[error("machine `{0}` not found in project")]
    UnknownMachine(String),

    #[error("agent runner `{0}` not declared in project")]
    UnknownRunner(String),

    #[error("external command failed: {cmd}: {status}\n--- stderr ---\n{stderr}")]
    Command {
        cmd: String,
        status: String,
        stderr: String,
    },

    /// A verification read could not confirm a state that one of Shelbi's own
    /// authoritative writes already established — the canonical case is
    /// GitHub's eventually-consistent PR API still reporting an obsolete head
    /// commit seconds after a `git push` that the branch ref already reflects.
    /// This is retry-safe: re-running the same (idempotent) command once the
    /// read propagates succeeds. It is deliberately distinct from a hard
    /// provenance mismatch ([`Error::Other`]) so callers can map "retry me"
    /// and "never merge this" to different exit codes.
    #[error("{0}")]
    TransientVerification(String),

    #[error("unknown task id(s) in depends_on: {0}")]
    UnknownDepends(String),

    #[error("dependency cycle: {0}")]
    DependencyCycle(String),

    #[error("invalid workflow: {0}")]
    InvalidWorkflow(String),

    #[error("invalid statuses.yaml: {0}")]
    InvalidProjectStatuses(String),

    /// A workflow's `git:` block references one or more `{{var}}`
    /// placeholders that aren't present in the task's frontmatter
    /// parameters. The message is hand-tuned (singular vs. plural,
    /// concrete example) so the user immediately knows what to add —
    /// see `Plans/workflows.md` §12 "Parameterization".
    #[error("{}", missing_task_params_message(.workflow, .params))]
    MissingTaskParams {
        workflow: String,
        params: Vec<String>,
    },

    /// A split project YAML (in-repo mode) has a key that belongs on the
    /// other side of the split — e.g. `machines:` in the shared file, or
    /// `zen:` in the user-local file. The message names the field and
    /// points at the correct file so the fix is obvious. See
    /// `Plans/in-repo-vs-global-project-config.md` §3.
    #[error(
        "project YAML field `{field}` is in the {found_in} file but belongs \
         in the {expected_in} file; move it to the {expected_in} YAML"
    )]
    MisplacedProjectField {
        field: String,
        found_in: &'static str,
        expected_in: &'static str,
    },

    /// A `git:` block declares both `branch` and `branch_prefix`. They are
    /// mutually exclusive branch-naming strategies — a block picks one. The
    /// message names both keys and the offending workflow/project so the fix
    /// is unambiguous.
    #[error(
        "{scope}: `git.branch` and `git.branch_prefix` are mutually exclusive \
         — set one, not both"
    )]
    GitBranchConflict { scope: String },

    /// A project's `issue_tracker` block selects a backend whose required
    /// connection facts are missing or malformed. The message names the
    /// offending field (e.g. `issue_tracker.github.repo`) so the fix is
    /// unambiguous. See `Plans/pluggable-task-stores.md` §2 + D1.
    #[error("{0}")]
    InvalidIssueTracker(String),

    /// A project's `issue_tracker` block selects a valid remote backend
    /// (`github` / `jira` / `linear`) that parses and validates but has no live
    /// store implementation yet. Distinct from [`Error::InvalidIssueTracker`]
    /// (a config error the user must fix) so a caller can tell "not built yet"
    /// apart from "you configured it wrong". Only `file_system` resolves today.
    #[error(
        "issue_tracker backend `{0}` is not yet implemented \
         (only `file_system` is available today)"
    )]
    IssueTrackerUnimplemented(String),

    /// No auth token could be resolved for a remote issue-tracker backend.
    /// The resolver walked its whole chain — `GH_TOKEN`/`GITHUB_TOKEN` env,
    /// the `gh` CLI keychain, then the out-of-repo `tokens.yml` — and found
    /// nothing, so the message names every place it looked *and* the one-line
    /// fix. Shelbi never holds a long-lived secret on disk, so the default
    /// remedy is to reuse `gh`'s own auth. See `Plans/pluggable-task-stores.md`
    /// §4 + D2.
    #[error(
        "no auth token found for the `{backend}` issue-tracker backend: \
         checked $GH_TOKEN, $GITHUB_TOKEN, `gh auth token`, and {token_file}; \
         run `gh auth login` or set GH_TOKEN"
    )]
    MissingIssueTrackerAuth {
        backend: &'static str,
        token_file: String,
    },

    /// The `gh auth token` keychain probe kept failing across its retries, so a
    /// token could not be read from `gh` — and neither the `GH_TOKEN` /
    /// `GITHUB_TOKEN` env nor the out-of-repo `tokens.yml` supplied one either.
    /// Distinct from [`Error::MissingIssueTrackerAuth`] so a transient keychain
    /// stall (the macOS `security` / `securityd` throttle that gives up around
    /// 6 s) is reported as what it is — a probe that timed out — instead of "no
    /// token found", which sends the operator to `gh auth login` for a login
    /// that is actually fine. Carries the probe's exit status, elapsed time, and
    /// stderr tail so the real cause is legible. See
    /// `Plans/pluggable-task-stores.md` §4 + D2.
    /// Boxed so this diagnostic-heavy variant (five strings) does not bloat the
    /// whole `Error` enum — and, through it, every `Result` in the workspace —
    /// past clippy's `result_large_err` bound.
    #[error("{0}")]
    GhTokenProbeFailed(Box<GhTokenProbeFailure>),

    /// An out-of-repo `tokens.yml` exists but its filesystem permissions are
    /// looser than `0600`, so a secret is readable by group/other. Refusing to
    /// read it (rather than silently trusting a world-readable secret) mirrors
    /// the plan's "belt and suspenders" stance on token files. The message
    /// names the file and the exact `chmod` fix.
    #[error(
        "token file {path} is readable by group/other (mode {mode:04o}); \
         a token must stay private — run `chmod 600 {path}`"
    )]
    InsecureTokenFile { path: String, mode: u32 },

    /// A GitHub issue's hand-edited fenced shelbi metadata block could not be
    /// parsed (unparseable inner YAML, or a `<!-- shelbi:begin -->` marker with
    /// no matching `<!-- shelbi:end -->`). A write path refuses with this rather
    /// than clobbering the human's edit with default metadata; the issue body on
    /// GitHub is left exactly as written. The message names the issue and the
    /// parse detail so the user knows where to look. Read paths never raise this
    /// — they render the issue with empty metadata and a warning instead.
    #[error(
        "issue `{id}` has a malformed shelbi metadata block ({detail}); \
         fix the fenced `<!-- shelbi:begin -->` block on github.com and retry"
    )]
    MalformedIssueMetadata { id: String, detail: String },

    #[error("{0}")]
    Other(String),
}

/// The diagnostic payload of [`Error::GhTokenProbeFailed`]: the exit status,
/// elapsed time, and stderr tail of a `gh auth token` probe that exhausted its
/// retries, plus the `tokens.yml` path that was also checked. Its `Display` is
/// the operator-facing sentence — a transient macOS keychain stall reported as
/// what it is, not as "no token found". Boxed inside the `Error` variant.
#[derive(Debug, Clone)]
pub struct GhTokenProbeFailure {
    /// How many `gh auth token` spawns were attempted (first plus retries).
    pub attempts: u32,
    /// The last attempt's process exit status, e.g. `exit status: 1`.
    pub exit_status: String,
    /// The last attempt's wall-clock duration, pre-rendered (e.g. `6.0s`).
    pub elapsed: String,
    /// The tail of the last attempt's stderr, e.g. `no oauth token found for
    /// github.com`.
    pub stderr_tail: String,
    /// The out-of-repo `tokens.yml` path that was also checked and missed.
    pub token_file: String,
}

impl std::fmt::Display for GhTokenProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`gh auth token` failed after {} attempt(s) (exit {} in {}): {}; \
             the token could not be read from the OS keychain — a transient \
             macOS keychain stall clears on its own, and shelbi retries — and \
             no $GH_TOKEN/$GITHUB_TOKEN or {} was set; \
             if this persists, check `gh auth status`",
            self.attempts, self.exit_status, self.elapsed, self.stderr_tail, self.token_file,
        )
    }
}

impl Error {
    /// True for a read that could not yet confirm one of Shelbi's own
    /// authoritative writes (see [`Error::TransientVerification`]). Callers
    /// use this to map retry-safe failures to a distinct exit code instead of
    /// a hard "do not merge" failure.
    pub fn is_transient(&self) -> bool {
        matches!(self, Error::TransientVerification(_))
    }
}

fn missing_task_params_message(workflow: &str, params: &[String]) -> String {
    match params {
        [] => format!("workflow `{workflow}` requires unknown parameters"),
        [one] => format!(
            "workflow `{workflow}` requires parameter `{one}`; \
             add `{one}: <value>` to the task frontmatter"
        ),
        many => {
            let list = many
                .iter()
                .map(|p| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let first = &many[0];
            format!(
                "workflow `{workflow}` requires parameters {list}; \
                 add them to the task frontmatter (e.g. `{first}: <value>`)"
            )
        }
    }
}
