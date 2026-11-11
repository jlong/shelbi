//! `shelbi remind` — reminders in the awareness store
//! (`shelbi_state::awareness`). The hub poller writes a `reminder` line to
//! `events.log` when one comes due, so the orchestrator is told instead of
//! having to poll.

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use clap::Subcommand;
use shelbi_state::awareness::{self, Reminder};

use super::require_registered_project;

#[derive(Debug, Subcommand)]
pub enum RemindCmd {
    /// Remind me after a duration: `shelbi remind in 1h "check the tap PR"`.
    In {
        /// How long from now: `30s`, `10m`, `1h`, `1h30m`.
        duration: String,
        /// What to be reminded of (the words are joined, so quoting is optional).
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Repeat every DURATION after the first (at least 10m).
        #[arg(long, value_name = "DURATION")]
        every: Option<String>,
    },
    /// Remind me at a time: `15:00` (local; tomorrow if already past),
    /// `2026-11-12 09:30` (local), or RFC 3339.
    At {
        time: String,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Repeat every DURATION after the first (at least 10m).
        #[arg(long, value_name = "DURATION")]
        every: Option<String>,
    },
    /// List pending reminders, soonest first.
    List {
        /// Print them as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Cancel a pending reminder.
    Cancel { id: String },
}

pub fn run(project_opt: Option<String>, cmd: RemindCmd) -> Result<()> {
    let project = require_registered_project(project_opt)?;
    match cmd {
        RemindCmd::In { duration, text, every } => {
            let d = shelbi_core::parse_duration(&duration).map_err(|e| anyhow!(e))?;
            let due = Utc::now() + chrono::Duration::from_std(d)?;
            add(&project, due, &text, every.as_deref())
        }
        RemindCmd::At { time, text, every } => {
            let due = parse_at(&time, Local::now())?;
            add(&project, due, &text, every.as_deref())
        }
        RemindCmd::List { json } => {
            let mut reminders = awareness::read_awareness(&project)
                .map_err(|e| anyhow!(e))?
                .reminders;
            reminders.sort_by_key(|r| r.due_at);
            if json {
                println!("{}", serde_json::to_string_pretty(&reminders)?);
            } else {
                print!("{}", render_list(&reminders, Utc::now()));
            }
            Ok(())
        }
        RemindCmd::Cancel { id } => match awareness::cancel_reminder(&project, &id).map_err(|e| anyhow!(e))? {
            Some(r) => {
                println!("✓ canceled reminder {}: {}", r.id, r.text);
                Ok(())
            }
            None => bail!("no reminder `{id}` (see `shelbi remind list`)"),
        },
    }
}

fn add(project: &str, due: DateTime<Utc>, text: &[String], every: Option<&str>) -> Result<()> {
    let every = every
        .map(shelbi_core::parse_duration)
        .transpose()
        .map_err(|e| anyhow!(e))?;
    let r = awareness::add_reminder(project, due, &text.join(" "), every).map_err(|e| anyhow!(e))?;
    let repeat = r
        .every
        .map(|e| format!(", then every {}", shelbi_core::format_duration(e)))
        .unwrap_or_default();
    println!(
        "✓ reminder {} due {} ({}{repeat})",
        r.id,
        local_stamp(r.due_at),
        relative(r.due_at, Utc::now())
    );
    Ok(())
}

/// Parse a `remind at` time against `now` (local). A bare time of day that has
/// already passed today means tomorrow.
fn parse_at<Tz: TimeZone>(s: &str, now: DateTime<Tz>) -> Result<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    let tz = now.timezone();
    let localize = |naive: NaiveDateTime| -> Result<DateTime<Utc>> {
        tz.from_local_datetime(&naive)
            .earliest()
            .map(|t| t.with_timezone(&Utc))
            .ok_or_else(|| anyhow!("`{s}` does not exist in the local time zone"))
    };
    for fmt in ["%Y-%m-%d %H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return localize(naive);
        }
    }
    for fmt in ["%H:%M", "%H:%M:%S"] {
        if let Ok(time) = NaiveTime::parse_from_str(s, fmt) {
            let today: NaiveDate = now.naive_local().date();
            let mut due = localize(today.and_time(time))?;
            if due <= now.with_timezone(&Utc) {
                let tomorrow = today.succ_opt().ok_or_else(|| anyhow!("date out of range"))?;
                due = localize(tomorrow.and_time(time))?;
            }
            return Ok(due);
        }
    }
    bail!("can't read `{s}` as a time: use `15:00`, `2026-11-12 09:30`, or RFC 3339")
}

fn local_stamp(t: DateTime<Utc>) -> String {
    let local = t.with_timezone(&Local);
    if local.date_naive() == Local::now().date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// `in 1h5m` / `5m ago` / `now`, to the minute once past a minute.
pub(crate) fn relative(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (t - now).num_seconds();
    let round = |s: u64| if s >= 60 { s / 60 * 60 } else { s };
    match secs {
        0 => "now".into(),
        s if s > 0 => format!(
            "in {}",
            shelbi_core::format_duration(std::time::Duration::from_secs(round(s as u64)))
        ),
        s => format!(
            "{} ago",
            shelbi_core::format_duration(std::time::Duration::from_secs(round(s.unsigned_abs())))
        ),
    }
}

fn render_list(reminders: &[Reminder], now: DateTime<Utc>) -> String {
    if reminders.is_empty() {
        return "no pending reminders\n".into();
    }
    let mut out = String::new();
    for r in reminders {
        let every = r
            .every
            .map(|e| format!("  (every {})", shelbi_core::format_duration(e)))
            .unwrap_or_default();
        out.push_str(&format!(
            "{:<5} {}  {:<10} {}{every}\n",
            r.id,
            local_stamp(r.due_at),
            relative(r.due_at, now),
            r.text
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn local(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    #[test]
    fn at_reads_a_time_of_day_rolling_to_tomorrow_once_past() {
        let now = local("2026-11-10T14:00:00-08:00");
        assert_eq!(
            parse_at("15:00", now).unwrap(),
            local("2026-11-10T15:00:00-08:00").with_timezone(&Utc)
        );
        assert_eq!(
            parse_at("09:30", now).unwrap(),
            local("2026-11-11T09:30:00-08:00").with_timezone(&Utc)
        );
        assert_eq!(
            parse_at("2026-11-12 09:30", now).unwrap(),
            local("2026-11-12T09:30:00-08:00").with_timezone(&Utc)
        );
        assert_eq!(
            parse_at("2026-11-12T09:30:00Z", now).unwrap(),
            local("2026-11-12T09:30:00Z").with_timezone(&Utc)
        );
        assert!(parse_at("soon", now).is_err());
    }

    #[test]
    fn relative_times_read_naturally() {
        let now = Utc::now();
        assert_eq!(relative(now + chrono::Duration::seconds(3_930), now), "in 1h5m");
        assert_eq!(relative(now - chrono::Duration::seconds(300), now), "5m ago");
        assert_eq!(relative(now + chrono::Duration::seconds(20), now), "in 20s");
    }
}
