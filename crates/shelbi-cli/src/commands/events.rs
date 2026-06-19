//! `shelbi events <subcommand>` — read-only views over
//! `~/.shelbi/events.log`, the append-only transition log written by the
//! hub's worker-state poller.
//!
//! Hub-global, not per-project: every line is `<rfc3339> worker=<name>
//! <prev> -> <new>`. Filtering by project is out of scope here — the
//! orchestrator can grep its own workers out of the stream.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum EventsCmd {
    /// Print the most recent worker-state transitions and optionally
    /// follow new ones as they're appended.
    Tail {
        /// Number of trailing lines to print before following (or
        /// before exiting if `--follow` is not set).
        #[arg(short = 'n', long = "lines", default_value_t = 20)]
        lines: usize,
        /// Only show events newer than this. Accepts a relative
        /// duration like `10m`, `2h`, `1d`. When set, `-n` is ignored
        /// and *all* matching lines are printed.
        #[arg(long)]
        since: Option<String>,
        /// Stream new transitions as they're appended. Exit on Ctrl-C.
        #[arg(short = 'f', long)]
        follow: bool,
    },
}

pub fn run(cmd: EventsCmd) -> Result<()> {
    match cmd {
        EventsCmd::Tail { lines, since, follow } => tail(lines, since, follow),
    }
}

fn tail(lines: usize, since: Option<String>, follow: bool) -> Result<()> {
    let path = shelbi_state::events_log_path().map_err(|e| anyhow!(e))?;

    let (initial, end_offset) = match fs::read(&path) {
        Ok(buf) => {
            let len = buf.len() as u64;
            let text = String::from_utf8_lossy(&buf).into_owned();
            let filtered: Vec<&str> = if let Some(spec) = since.as_deref() {
                let cutoff = Utc::now()
                    - chrono::Duration::from_std(parse_duration(spec)?)
                        .map_err(|e| anyhow!("duration `{spec}` out of range: {e}"))?;
                text.lines()
                    .filter(|line| line_after(line, cutoff))
                    .collect()
            } else {
                let all: Vec<&str> = text.lines().collect();
                let start = all.len().saturating_sub(lines);
                all[start..].to_vec()
            };
            let initial = filtered.iter().map(|l| (*l).to_string()).collect::<Vec<_>>();
            (initial, len)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), 0),
        Err(e) => return Err(anyhow::Error::new(e).context("reading events.log")),
    };

    for line in &initial {
        println!("{line}");
    }

    if !follow {
        return Ok(());
    }

    follow_from(&path, end_offset)
}

/// `tail -f` shape: park at `start_offset` and print any newly appended
/// bytes. Polling cadence (250ms) matches the worker poller's typical
/// inter-tick gap — fast enough to feel live, slow enough that a 10-worker
/// project barely touches the filesystem.
fn follow_from(path: &std::path::PathBuf, start_offset: u64) -> Result<()> {
    let interval = Duration::from_millis(250);
    let mut offset = start_offset;
    let mut pending = String::new();
    loop {
        thread::sleep(interval);

        let mut file = match fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(anyhow::Error::new(e).context("opening events.log")),
        };
        let len = file
            .metadata()
            .context("stat events.log")?
            .len();

        // File was truncated or rotated — restart from the top so we
        // don't silently drop the new content the next writer added.
        if len < offset {
            offset = 0;
            pending.clear();
        }
        if len == offset {
            continue;
        }
        file.seek(SeekFrom::Start(offset))
            .context("seek events.log")?;
        let mut buf = Vec::with_capacity((len - offset) as usize);
        file.read_to_end(&mut buf).context("read events.log")?;
        offset = len;

        pending.push_str(&String::from_utf8_lossy(&buf));
        // Hold back the final fragment until its newline arrives so we
        // never print a half-written event.
        while let Some(nl) = pending.find('\n') {
            let line = pending[..nl].to_string();
            pending.drain(..=nl);
            if !line.is_empty() {
                println!("{line}");
            }
        }
    }
}

/// Return true when the event line's leading RFC3339 timestamp is at or
/// after `cutoff`. Lines that fail to parse are kept — better to over-
/// include than to silently drop on a future format tweak.
fn line_after(line: &str, cutoff: DateTime<Utc>) -> bool {
    let Some(ts_str) = line.split_whitespace().next() else {
        return true;
    };
    match DateTime::parse_from_rfc3339(ts_str) {
        Ok(t) => t.with_timezone(&Utc) >= cutoff,
        Err(_) => true,
    }
}

/// Parse `10s`, `5m`, `2h`, `1d` (and bare seconds) into a [`Duration`].
/// Intentionally narrow: this is a CLI filter, not a humantime port.
pub(crate) fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return Err(anyhow!("empty duration"));
    }
    let (num_part, unit) = match s.chars().last().unwrap() {
        c if c.is_ascii_digit() => (s, 's'),
        c => (&s[..s.len() - c.len_utf8()], c),
    };
    let n: u64 = num_part
        .parse()
        .map_err(|_| anyhow!("`{s}` is not a duration like 10m, 2h, 1d"))?;
    let mult: u64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        other => return Err(anyhow!("unknown duration unit `{other}` (use s/m/h/d)")),
    };
    let secs = n
        .checked_mul(mult)
        .ok_or_else(|| anyhow!("duration `{s}` overflows"))?;
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parse_duration_units() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86_400));
        // Bare integer is treated as seconds.
        assert_eq!(parse_duration("45").unwrap(), Duration::from_secs(45));
    }

    #[test]
    fn parse_duration_rejects_garbage() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("10x").is_err());
        assert!(parse_duration("m").is_err());
    }

    #[test]
    fn line_after_filters_old_timestamps() {
        let cutoff = Utc.with_ymd_and_hms(2026, 6, 19, 12, 0, 0).unwrap();
        let old = "2026-06-19T11:59:59+00:00 worker=alpha none -> working";
        let recent = "2026-06-19T12:00:01+00:00 worker=alpha working -> awaiting_input";
        assert!(!line_after(old, cutoff));
        assert!(line_after(recent, cutoff));
    }

    #[test]
    fn line_after_keeps_unparseable_lines() {
        let cutoff = Utc.with_ymd_and_hms(2026, 6, 19, 12, 0, 0).unwrap();
        // Garbage at the head — keep, don't silently drop.
        assert!(line_after("bogus line", cutoff));
        assert!(line_after("", cutoff));
    }
}
