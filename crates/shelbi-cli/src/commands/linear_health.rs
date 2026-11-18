//! The Linear section of `shelbi status --full` and `shelbi doctor`: the API
//! key, the account and filters, the key's two rate-limit buckets, the last
//! board refresh, and the observed request rate. The Linear counterpart of the
//! GitHub budget block; both commands render from one [`LinearHealth`]
//! snapshot.
//!
//! Every read is local and side-effect free: the key's budget file and the
//! hub-wide request log ([`shelbi_state::linear_client`]), the published board
//! index (account and freshness) and the project's config (filters). Resolving
//! the key may read the macOS keychain, the same cost the GitHub section's
//! token probe pays; nothing calls Linear.

use std::time::Duration;

use chrono::{DateTime, Utc};
use shelbi_core::LinearConnection;
use shelbi_state::linear_client::{self, BucketSnapshot, LinearBudget, RequestLogEntry};

/// How far back the request rate is read.
pub(crate) const RATE_WINDOW: Duration = Duration::from_secs(3_600);

/// What the Linear section shows for one project.
pub(crate) struct LinearHealth {
    /// The project's filters (team plus the optional ones).
    pub conn: LinearConnection,
    /// Where the API key was found, or `None` when there is none.
    pub key_source: Option<String>,
    /// The key's budget file, when the key resolved.
    pub budget: Option<LinearBudget>,
    /// The key owner's display name, from the last board read.
    pub account: Option<String>,
    /// When the daemon last published the board.
    pub last_refresh: Option<String>,
    /// The daemon's last refresh failure, if the latest tick failed.
    pub refresh_error: Option<shelbi_state::BoardRefreshError>,
    /// Hub-wide Linear request-log entries in the last [`RATE_WINDOW`].
    pub requests: Vec<RequestLogEntry>,
}

impl LinearHealth {
    /// Gather the snapshot for `project`; `None` unless its backend is Linear.
    pub fn read(project: &str, now: DateTime<Utc>) -> Option<Self> {
        let cfg = shelbi_state::load_project(project).ok()?.issue_tracker;
        if cfg.backend != shelbi_core::IssueTrackerBackend::Linear {
            return None;
        }
        let key = linear_client::resolve_linear_key(Some(project)).ok();
        let index = shelbi_state::read_valid_board_index(
            project,
            shelbi_state::expected_board_repo(&cfg).as_deref(),
        );
        Some(LinearHealth {
            conn: cfg.linear.unwrap_or_default(),
            key_source: key.as_ref().map(|k| k.source().to_string()),
            budget: key.as_ref().map(|k| {
                linear_client::read_budget(&shelbi_state::gh_budget::token_key(k.expose()))
            }),
            account: index.as_ref().and_then(|i| i.viewer_login.clone()),
            last_refresh: index.map(|i| i.fetched_at),
            refresh_error: shelbi_state::read_board_refresh_error(project),
            requests: linear_client::recent_requests(RATE_WINDOW, now),
        })
    }

    /// `team ENG · project Shelbi · label shelbi · assignee me`.
    pub fn filters_line(&self) -> String {
        let mut parts = vec![format!("team {}", self.conn.team_ref().unwrap_or("(unset)"))];
        for (name, value) in [
            ("project", self.conn.project_ref()),
            ("label", self.conn.label_ref()),
            ("assignee", self.conn.assignee_ref()),
        ] {
            if let Some(v) = value {
                parts.push(format!("{name} {v}"));
            }
        }
        parts.join(" · ")
    }

    /// `from the macOS keychain`, or the fix when there is no key.
    pub fn key_line(&self) -> String {
        match &self.key_source {
            Some(source) => format!("from {source}"),
            None => format!(
                "missing; run `shelbi tracker linear login` (or set ${})",
                linear_client::LINEAR_API_KEY_ENV
            ),
        }
    }

    /// The park-until time when the key is still parked.
    pub fn parked_until(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.budget.as_ref().and_then(|b| b.parked_at(now))
    }

    /// Requests that reached Linear (everything but a fail-fast while parked).
    pub fn sent(&self) -> impl Iterator<Item = &RequestLogEntry> {
        self.requests.iter().filter(|e| e.outcome != "err:parked")
    }

    /// Sent requests that failed (an HTTP error or a classified failure).
    pub fn failed(&self) -> impl Iterator<Item = &RequestLogEntry> {
        self.sent().filter(|e| e.outcome != "ok")
    }
}

/// `1400 of 1500 remaining, resets 10:15`, or `unknown` before any read.
pub(crate) fn bucket_line(bucket: &BucketSnapshot) -> String {
    let remaining = match (bucket.remaining, bucket.limit) {
        (Some(r), Some(l)) => format!("{r} of {l} remaining"),
        (Some(r), None) => format!("{r} remaining"),
        _ => "remaining unknown".to_string(),
    };
    match bucket.reset_at.and_then(format_epoch_local) {
        Some(hm) => format!("{remaining}, resets {hm}"),
        None => remaining,
    }
}

/// Format an epoch (seconds) as a local `HH:MM`.
pub(crate) fn format_epoch_local(epoch: i64) -> Option<String> {
    DateTime::from_timestamp(epoch, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
}

/// Failed attempts grouped by outcome: `err:timeout (2), http500 (1)`.
pub(crate) fn failure_classes<'a>(failed: impl Iterator<Item = &'a RequestLogEntry>) -> String {
    use std::collections::BTreeMap;
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    for e in failed {
        let class = e.outcome.strip_prefix("err:").unwrap_or(&e.outcome);
        *by.entry(class).or_insert(0) += 1;
    }
    by.iter().map(|(c, n)| format!("{c} ({n})")).collect::<Vec<_>>().join(", ")
}

/// The `## Linear` body for `shelbi status --full`.
pub(crate) fn status_section(
    h: &LinearHealth,
    freshness: Option<&shelbi_state::BoardFreshness>,
    now: DateTime<Utc>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "account: {}\n",
        h.account.as_deref().unwrap_or("unknown (no board read yet)")
    ));
    out.push_str(&format!("api key: {}\n", h.key_line()));
    out.push_str(&format!("filters: {}\n", h.filters_line()));
    match freshness {
        Some(f) if h.last_refresh.is_some() => {
            let age = f
                .age_secs
                .map(|s| format!("{s}s old"))
                .unwrap_or_else(|| "age unknown".to_string());
            let word = match f.source {
                shelbi_state::BoardSource::CacheFallback => "cache fallback (no daemon)",
                _ if f.stale => "stale",
                _ => "warm",
            };
            out.push_str(&format!("board index: {age} ({word})\n"));
        }
        _ if h.refresh_error.is_some() => out.push_str("board index: never published\n"),
        _ => out.push_str("board index: (no index published)\n"),
    }
    if let Some(at) = &h.last_refresh {
        out.push_str(&format!("last refresh: {at}\n"));
    }
    if let Some(err) = &h.refresh_error {
        out.push_str(&format!("last error: {} (at {})\n", err.error, err.at));
    }
    let budget = h.budget.clone().unwrap_or_default();
    out.push_str(&format!("requests budget: {}\n", bucket_line(&budget.requests)));
    out.push_str(&format!("complexity budget: {}\n", bucket_line(&budget.complexity)));
    if let Some(until) = h.parked_until(now) {
        out.push_str(&format!(
            "rate limited: requests paused until {}\n",
            until.with_timezone(&chrono::Local).format("%H:%M")
        ));
    }
    let sent = h.sent().count();
    let failed = h.failed().count();
    let failed_str = if failed > 0 { format!(" ({failed} failed)") } else { String::new() };
    out.push_str(&format!(
        "request rate: {sent} request{} in the last hour{failed_str} (all Linear projects on this hub)\n",
        if sent == 1 { "" } else { "s" },
    ));
    out
}

/// The one-liner for bare `shelbi status`: `board 12s · linear 1400 left`.
pub(crate) fn summary_suffix(h: &LinearHealth) -> Option<String> {
    let (bucket, snap) = h.budget.as_ref()?.tightest()?;
    let remaining = snap.remaining?;
    Some(match bucket {
        linear_client::LinearBucket::Requests => format!("linear {remaining} left"),
        linear_client::LinearBucket::Complexity => format!("linear {remaining} complexity left"),
    })
}
