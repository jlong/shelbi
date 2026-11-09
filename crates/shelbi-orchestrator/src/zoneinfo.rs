//! IANA time zones read from the system's compiled zone files.
//!
//! Shelbi needs one zone lookup: the reset hint Claude prints on its
//! usage-limit modal names an IANA zone (`7:20am (America/Los_Angeles)`).
//! Rather than compile the whole tz database into the binary, this reads
//! `<TZDIR or /usr/share/zoneinfo>/<name>` (RFC 8536 TZif) and, for instants
//! past the file's last transition (Linux "slim" files store almost no
//! transitions), evaluates the POSIX TZ footer (`PST8PDT,M3.2.0,M11.1.0`).
//!
//! A name that fails validation, a missing file, or anything we can't parse
//! yields `None`: callers treat that as "unknown zone, don't guess".

use chrono::{DateTime, Datelike, LocalResult, NaiveDate, NaiveDateTime, Utc};

/// Largest zone file we'll read. Real ones are a few KB.
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// A zone offset never exceeds a day in either direction (POSIX caps the
/// hour field at 24, plus minutes and seconds), so every UTC instant that
/// can map to a given local time lies within this many seconds of it.
const MAX_OFFSET_SECS: i64 = 26 * 3600;

/// One zone: its transition table plus the optional rule for later instants.
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    /// UTC instants (seconds since the epoch) where the offset changes,
    /// ascending, each paired with the offset (seconds east of UTC) in force
    /// from that instant on.
    transitions: Vec<(i64, i32)>,
    /// Offset before the first transition (time type 0).
    initial: i32,
    /// Rule for instants at or after the last transition, if the file has one.
    rule: Option<PosixTz>,
}

impl Zone {
    /// Load `name` from `$TZDIR` (when set and non-empty) or
    /// `/usr/share/zoneinfo`. `None` for an invalid name, a missing or
    /// unreadable file, or one we can't parse.
    pub fn load(name: &str) -> Option<Zone> {
        let dir = std::env::var_os("TZDIR")
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "/usr/share/zoneinfo".into());
        Zone::load_from(std::path::Path::new(&dir), name)
    }

    /// [`Zone::load`] against an explicit zone directory.
    fn load_from(dir: &std::path::Path, name: &str) -> Option<Zone> {
        if !valid_name(name) {
            return None;
        }
        let path = dir.join(name);
        let file = std::fs::File::open(path).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(file, MAX_FILE_BYTES + 1),
            &mut bytes,
        )
        .ok()?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return None;
        }
        Zone::parse_tzif(&bytes)
    }

    /// Parse TZif bytes (RFC 8536). Uses the 64-bit section of a v2+ file;
    /// a v1 file has only 32-bit data and no footer.
    pub fn parse_tzif(bytes: &[u8]) -> Option<Zone> {
        let mut r = Reader { bytes, pos: 0 };
        let v1 = Header::read(&mut r)?;
        if v1.version == 0 {
            return Zone::read_block(&mut r, &v1, 4, None);
        }
        // Skip the v1 block; the v2+ block repeats it with 64-bit times.
        r.skip(v1.block_len(4))?;
        let v2 = Header::read(&mut r)?;
        let start = r.pos;
        r.skip(v2.block_len(8))?;
        // Footer: "\n<TZ string>\n".
        if r.take(1)? != b"\n" {
            return None;
        }
        let rest = &bytes[r.pos..];
        let end = rest.iter().position(|&b| b == b'\n')?;
        let footer = std::str::from_utf8(&rest[..end]).ok()?;
        let rule = if footer.is_empty() {
            None
        } else {
            Some(PosixTz::parse(footer)?)
        };
        let mut block = Reader { bytes, pos: start };
        Zone::read_block(&mut block, &v2, 8, rule)
    }

    fn read_block(
        r: &mut Reader,
        h: &Header,
        time_len: usize,
        rule: Option<PosixTz>,
    ) -> Option<Zone> {
        // Leap-second ("right/") files count TAI-ish time; we'd misplace
        // every instant, so treat them as unknown rather than guess.
        if h.leapcnt != 0 || h.typecnt == 0 {
            return None;
        }
        // Check the counts against the bytes actually present before
        // allocating anything sized by them.
        if r.bytes.len() - r.pos < h.block_len(time_len) {
            return None;
        }
        let mut times = Vec::with_capacity(h.timecnt);
        for _ in 0..h.timecnt {
            let raw = r.take(time_len)?;
            times.push(if time_len == 4 {
                i32::from_be_bytes(raw.try_into().ok()?) as i64
            } else {
                i64::from_be_bytes(raw.try_into().ok()?)
            });
        }
        let indices = r.take(h.timecnt)?.to_vec();
        let mut offsets = Vec::with_capacity(h.typecnt);
        for _ in 0..h.typecnt {
            let ttinfo = r.take(6)?;
            let utoff = i32::from_be_bytes(ttinfo[..4].try_into().ok()?);
            // RFC 8536: -2^31 is invalid; offsets stay well inside a day.
            if (utoff as i64).abs() > MAX_OFFSET_SECS {
                return None;
            }
            offsets.push(utoff);
        }
        let mut transitions = Vec::with_capacity(h.timecnt);
        for (t, idx) in times.into_iter().zip(indices) {
            let offset = *offsets.get(idx as usize)?;
            if transitions.last().is_some_and(|&(prev, _)| prev >= t) {
                return None;
            }
            transitions.push((t, offset));
        }
        Some(Zone {
            transitions,
            initial: offsets[0],
            rule,
        })
    }

    /// Offset (seconds east of UTC) in force at the UTC instant `t`.
    pub fn offset_at(&self, t: i64) -> i32 {
        let n = self.transitions.partition_point(|&(at, _)| at <= t);
        // Past the table (or with no table at all) the footer rule decides.
        if let (true, Some(rule)) = (n == self.transitions.len(), &self.rule) {
            return rule.offset_at(t);
        }
        match n {
            0 => self.initial,
            n => self.transitions[n - 1].1,
        }
    }

    /// The local calendar date in this zone at the UTC instant `at`.
    pub fn local_date(&self, at: DateTime<Utc>) -> NaiveDate {
        let local = at.timestamp() + self.offset_at(at.timestamp()) as i64;
        DateTime::from_timestamp(local, 0)
            .map(|dt| dt.date_naive())
            .unwrap_or_else(|| at.date_naive())
    }

    /// Every UTC instant whose local reading in this zone is `local`, in
    /// chrono's shape: `Single` normally, `Ambiguous(earlier, later)` in a
    /// fall-back overlap, `None` in a spring-forward gap.
    pub fn utc_from_local(&self, local: NaiveDateTime) -> LocalResult<DateTime<Utc>> {
        let l = local.and_utc().timestamp();
        let mut found: Vec<i64> = self
            .offsets_near(l)
            .into_iter()
            .map(|o| l - o as i64)
            .filter(|&t| l - self.offset_at(t) as i64 == t)
            .collect();
        found.sort_unstable();
        found.dedup();
        let at = |t: i64| DateTime::from_timestamp(t, 0);
        match found.as_slice() {
            [] => LocalResult::None,
            [t] => at(*t).map_or(LocalResult::None, LocalResult::Single),
            [first, .., last] => match (at(*first), at(*last)) {
                (Some(a), Some(b)) => LocalResult::Ambiguous(a, b),
                _ => LocalResult::None,
            },
        }
    }

    /// A superset of the offsets in force anywhere within a day of `l`
    /// (read as UTC), which covers every offset that could map some UTC
    /// instant to local time `l`.
    fn offsets_near(&self, l: i64) -> Vec<i32> {
        let (lo, hi) = (l - MAX_OFFSET_SECS, l + MAX_OFFSET_SECS);
        let mut offsets = vec![self.offset_at(lo)];
        let first = self.transitions.partition_point(|&(at, _)| at <= lo);
        let last = self.transitions.partition_point(|&(at, _)| at <= hi);
        offsets.extend(self.transitions[first..last].iter().map(|&(_, o)| o));
        let past_table = self.transitions.last().is_none_or(|&(at, _)| hi >= at);
        if let (true, Some(rule)) = (past_table, &self.rule) {
            offsets.push(rule.std);
            offsets.extend(rule.dst.as_ref().map(|d| d.offset));
        }
        offsets.sort_unstable();
        offsets.dedup();
        offsets
    }
}

/// `name` must look like a relative zone path: no traversal, no absolute
/// path, nothing outside `[A-Za-z0-9_+-/]`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-' | b'/'))
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let out = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(out)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }

    fn count(&mut self) -> Option<usize> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?) as usize)
    }
}

struct Header {
    version: u8,
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

impl Header {
    fn read(r: &mut Reader) -> Option<Header> {
        if r.take(4)? != b"TZif" {
            return None;
        }
        let version = r.take(1)?[0];
        let version = match version {
            0 => 0,
            b'2'..=b'9' => version - b'0',
            _ => return None,
        };
        r.skip(15)?;
        Some(Header {
            version,
            isutcnt: r.count()?,
            isstdcnt: r.count()?,
            leapcnt: r.count()?,
            timecnt: r.count()?,
            typecnt: r.count()?,
            charcnt: r.count()?,
        })
    }

    /// Bytes in the data block that follows this header.
    fn block_len(&self, time_len: usize) -> usize {
        self.timecnt * time_len
            + self.timecnt
            + self.typecnt * 6
            + self.charcnt
            + self.leapcnt * (time_len + 4)
            + self.isstdcnt
            + self.isutcnt
    }
}

/// A POSIX TZ string as found in a TZif footer, e.g. `PST8PDT,M3.2.0,M11.1.0`
/// or `<+09>-9`. Offsets are stored as seconds east of UTC (the string itself
/// counts west, so `PST8` is -28800).
#[derive(Clone, Debug, PartialEq)]
pub struct PosixTz {
    std: i32,
    dst: Option<Dst>,
}

#[derive(Clone, Debug, PartialEq)]
struct Dst {
    offset: i32,
    start: Rule,
    end: Rule,
}

/// When a DST change happens: a day of the year plus a local time of day
/// (seconds, possibly negative or past 24h per RFC 8536).
#[derive(Clone, Debug, PartialEq)]
struct Rule {
    day: Day,
    time: i32,
}

#[derive(Clone, Debug, PartialEq)]
enum Day {
    /// `Jn`: 1..=365, February 29 never counted.
    Julian1(u16),
    /// `n`: 0..=365, February 29 counted.
    Julian0(u16),
    /// `Mm.w.d`: weekday `d` (0 = Sunday) of week `w` (5 = last) of month `m`.
    WeekdayOfMonth { month: u8, week: u8, weekday: u8 },
}

impl PosixTz {
    pub fn parse(s: &str) -> Option<PosixTz> {
        let mut p = Parser {
            s: s.as_bytes(),
            pos: 0,
        };
        p.name()?;
        let std = -p.offset(24)?;
        if p.done() {
            return Some(PosixTz { std, dst: None });
        }
        p.name()?;
        let dst_offset = if p.peek().is_some_and(|c| c != b',') {
            -p.offset(24)?
        } else {
            std + 3600
        };
        // A footer with DST always carries its rule; POSIX leaves the
        // default implementation-defined, so don't guess one.
        p.expect(b',')?;
        let start = p.rule()?;
        p.expect(b',')?;
        let end = p.rule()?;
        if !p.done() {
            return None;
        }
        Some(PosixTz {
            std,
            dst: Some(Dst {
                offset: dst_offset,
                start,
                end,
            }),
        })
    }

    /// Offset (seconds east of UTC) at the UTC instant `t`.
    pub fn offset_at(&self, t: i64) -> i32 {
        let Some(dst) = &self.dst else {
            return self.std;
        };
        // Lay out the changes for the surrounding years and take the last
        // one at or before `t`. On a tie the start sorts after the end, so
        // a zone that is DST all year (end of year Y == start of Y+1) stays
        // in DST.
        let Some(year) = DateTime::from_timestamp(t + self.std as i64, 0).map(|d| d.year()) else {
            return self.std;
        };
        let mut changes = Vec::with_capacity(6);
        for y in year - 1..=year + 1 {
            // The start is written in standard time, the end in DST.
            if let Some(at) = dst.start.local_seconds(y) {
                changes.push((at - self.std as i64, true));
            }
            if let Some(at) = dst.end.local_seconds(y) {
                changes.push((at - dst.offset as i64, false));
            }
        }
        changes.sort_by_key(|&(at, is_start)| (at, is_start));
        match changes.iter().rev().find(|&&(at, _)| at <= t) {
            Some(&(_, true)) => dst.offset,
            Some(&(_, false)) => self.std,
            None => self.std,
        }
    }
}

impl Rule {
    /// The change in year `year` as local seconds since the epoch.
    fn local_seconds(&self, year: i32) -> Option<i64> {
        let jan1 = NaiveDate::from_ymd_opt(year, 1, 1)?;
        let date = match self.day {
            Day::Julian1(n) => {
                // Day 60 is always March 1, so in a leap year skip Feb 29.
                let leap = jan1.leap_year();
                let ordinal = n as u64 - 1 + u64::from(leap && n >= 60);
                jan1.checked_add_days(chrono::Days::new(ordinal))?
            }
            Day::Julian0(n) => jan1.checked_add_days(chrono::Days::new(n as u64))?,
            Day::WeekdayOfMonth {
                month,
                week,
                weekday,
            } => {
                let first = NaiveDate::from_ymd_opt(year, month as u32, 1)?;
                let first_wd = first.weekday().num_days_from_sunday() as u8;
                let mut day = 1 + (weekday + 7 - first_wd) % 7 + (week - 1) * 7;
                while NaiveDate::from_ymd_opt(year, month as u32, day as u32).is_none() {
                    day -= 7;
                }
                NaiveDate::from_ymd_opt(year, month as u32, day as u32)?
            }
        };
        Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp() + self.time as i64)
    }
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn done(&self) -> bool {
        self.pos == self.s.len()
    }

    fn expect(&mut self, c: u8) -> Option<()> {
        (self.peek()? == c).then(|| self.pos += 1)
    }

    /// `EST` (3+ letters) or `<+0530>` (3+ of alnum/+/-).
    fn name(&mut self) -> Option<()> {
        let start = self.pos;
        if self.peek() == Some(b'<') {
            self.pos += 1;
            let inner = self.pos;
            while self
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'-')
            {
                self.pos += 1;
            }
            let len = self.pos - inner;
            self.expect(b'>')?;
            return (len >= 3).then_some(());
        }
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        (self.pos - start >= 3).then_some(())
    }

    /// `[+-]hh[:mm[:ss]]` in seconds, sign applied; `max_hours` bounds hh.
    fn offset(&mut self, max_hours: i32) -> Option<i32> {
        let sign = match self.peek() {
            Some(b'-') => {
                self.pos += 1;
                -1
            }
            Some(b'+') => {
                self.pos += 1;
                1
            }
            _ => 1,
        };
        let hours = self.number(3)?;
        if hours > max_hours {
            return None;
        }
        let mut secs = hours * 3600;
        for scale in [60, 1] {
            if self.peek() != Some(b':') {
                break;
            }
            self.pos += 1;
            let part = self.number(2)?;
            if part > 59 {
                return None;
            }
            secs += part * scale;
        }
        Some(sign * secs)
    }

    fn number(&mut self, max_digits: usize) -> Option<i32> {
        let start = self.pos;
        while self.pos - start < max_digits && self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        std::str::from_utf8(&self.s[start..self.pos])
            .ok()?
            .parse()
            .ok()
    }

    /// `Jn`, `n`, or `Mm.w.d`, then an optional `/time` (default 02:00).
    fn rule(&mut self) -> Option<Rule> {
        let day = match self.peek()? {
            b'J' => {
                self.pos += 1;
                let n = self.number(3)?;
                if !(1..=365).contains(&n) {
                    return None;
                }
                Day::Julian1(n as u16)
            }
            b'M' => {
                self.pos += 1;
                let month = self.number(2)?;
                self.expect(b'.')?;
                let week = self.number(1)?;
                self.expect(b'.')?;
                let weekday = self.number(1)?;
                if !(1..=12).contains(&month) || !(1..=5).contains(&week) || weekday > 6 {
                    return None;
                }
                Day::WeekdayOfMonth {
                    month: month as u8,
                    week: week as u8,
                    weekday: weekday as u8,
                }
            }
            c if c.is_ascii_digit() => {
                let n = self.number(3)?;
                if n > 365 {
                    return None;
                }
                Day::Julian0(n as u16)
            }
            _ => return None,
        };
        let time = if self.peek() == Some(b'/') {
            self.pos += 1;
            // RFC 8536 extends the rule time to -167..=167 hours.
            self.offset(167)?
        } else {
            7200
        };
        Some(Rule { day, time })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp()
    }

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    fn zone(name: &str) -> Zone {
        Zone::load(name).unwrap_or_else(|| panic!("system zone {name} should load"))
    }

    fn single(r: LocalResult<DateTime<Utc>>) -> i64 {
        match r {
            LocalResult::Single(t) => t.timestamp(),
            other => panic!("expected one instant, got {other:?}"),
        }
    }

    #[test]
    fn los_angeles_across_both_dst_changes() {
        let la = zone("America/Los_Angeles");
        // 2025-03-09 02:00 PST -> 03:00 PDT, at 10:00 UTC.
        assert_eq!(la.offset_at(utc(2025, 3, 9, 9, 59)), -8 * 3600);
        assert_eq!(la.offset_at(utc(2025, 3, 9, 10, 0)), -7 * 3600);
        assert_eq!(
            la.utc_from_local(local(2025, 3, 9, 2, 30)),
            LocalResult::None
        );
        assert_eq!(
            single(la.utc_from_local(local(2025, 3, 9, 3, 0))),
            utc(2025, 3, 9, 10, 0)
        );
        // 2025-11-02 02:00 PDT -> 01:00 PST, at 09:00 UTC.
        assert_eq!(la.offset_at(utc(2025, 11, 2, 8, 59)), -7 * 3600);
        assert_eq!(la.offset_at(utc(2025, 11, 2, 9, 0)), -8 * 3600);
        assert_eq!(
            la.utc_from_local(local(2025, 11, 2, 1, 30)),
            LocalResult::Ambiguous(
                DateTime::from_timestamp(utc(2025, 11, 2, 8, 30), 0).unwrap(),
                DateTime::from_timestamp(utc(2025, 11, 2, 9, 30), 0).unwrap(),
            )
        );
        assert_eq!(
            single(la.utc_from_local(local(2025, 7, 1, 12, 0))),
            utc(2025, 7, 1, 19, 0)
        );
    }

    #[test]
    fn london_across_both_dst_changes() {
        let london = zone("Europe/London");
        // 2025-03-30 01:00 GMT -> 02:00 BST, at 01:00 UTC.
        assert_eq!(london.offset_at(utc(2025, 3, 30, 0, 59)), 0);
        assert_eq!(london.offset_at(utc(2025, 3, 30, 1, 0)), 3600);
        assert_eq!(
            london.utc_from_local(local(2025, 3, 30, 1, 30)),
            LocalResult::None
        );
        // 2025-10-26 02:00 BST -> 01:00 GMT, at 01:00 UTC.
        assert_eq!(london.offset_at(utc(2025, 10, 26, 0, 59)), 3600);
        assert_eq!(london.offset_at(utc(2025, 10, 26, 1, 0)), 0);
        assert!(matches!(
            london.utc_from_local(local(2025, 10, 26, 1, 30)),
            LocalResult::Ambiguous(a, b)
                if a.timestamp() == utc(2025, 10, 26, 0, 30) && b.timestamp() == utc(2025, 10, 26, 1, 30)
        ));
        assert_eq!(
            single(london.utc_from_local(local(2025, 1, 15, 9, 0))),
            utc(2025, 1, 15, 9, 0)
        );
    }

    #[test]
    fn tokyo_has_no_dst() {
        let tokyo = zone("Asia/Tokyo");
        for t in [
            utc(2025, 1, 1, 0, 0),
            utc(2025, 7, 1, 0, 0),
            utc(2090, 7, 1, 0, 0),
        ] {
            assert_eq!(tokyo.offset_at(t), 9 * 3600);
        }
        assert_eq!(
            single(tokyo.utc_from_local(local(2025, 3, 9, 2, 30))),
            utc(2025, 3, 8, 17, 30)
        );
    }

    #[test]
    fn far_future_instants_follow_the_footer_rule() {
        // Year 2200 is past every file's transition table (fat files stop at
        // 2037), so only the footer can answer.
        let la = zone("America/Los_Angeles");
        assert!(la.rule.is_some());
        assert!(la.transitions.last().unwrap().0 < utc(2200, 1, 1, 0, 0));
        // 2200-03-09 is the second Sunday of March; 2200-11-02 the first of
        // November.
        assert_eq!(la.offset_at(utc(2200, 3, 9, 9, 59)), -8 * 3600);
        assert_eq!(la.offset_at(utc(2200, 3, 9, 10, 0)), -7 * 3600);
        assert_eq!(la.offset_at(utc(2200, 11, 2, 8, 59)), -7 * 3600);
        assert_eq!(la.offset_at(utc(2200, 11, 2, 9, 0)), -8 * 3600);
        assert_eq!(
            la.utc_from_local(local(2200, 3, 9, 2, 30)),
            LocalResult::None
        );
        assert_eq!(
            single(la.utc_from_local(local(2200, 7, 4, 12, 0))),
            utc(2200, 7, 4, 19, 0)
        );
    }

    #[test]
    fn invalid_and_traversal_names_are_rejected() {
        for name in [
            "",
            "/etc/passwd",
            "../../etc/passwd",
            "America/../../etc/passwd",
            "America/New York",
            "America\\New_York",
            "America/New_York\0",
            "Europe/London;rm",
        ] {
            assert!(!valid_name(name), "{name:?} should be rejected");
            assert_eq!(Zone::load(name), None, "{name:?} should not load");
        }
        assert!(valid_name("Etc/GMT+5"));
        assert!(valid_name("America/Port-au-Prince"));
    }

    #[test]
    fn unknown_zones_and_directories_do_not_load() {
        assert_eq!(Zone::load("Mars/Olympus_Mons"), None);
        assert_eq!(Zone::load("America"), None);
        assert_eq!(Zone::load("ET"), None);
    }

    #[test]
    fn posix_footer_forms_parse() {
        let us = PosixTz::parse("PST8PDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(us.std, -8 * 3600);
        assert_eq!(us.dst.as_ref().unwrap().offset, -7 * 3600);

        let fixed = PosixTz::parse("<+0530>-5:30").unwrap();
        assert_eq!(
            fixed,
            PosixTz {
                std: 5 * 3600 + 1800,
                dst: None
            }
        );

        // Julian forms with explicit times, and negative/large RFC 8536 times.
        let j = PosixTz::parse("AAA3BBB,J60/1:30,300/-1").unwrap();
        let dst = j.dst.unwrap();
        assert_eq!(
            dst.start,
            Rule {
                day: Day::Julian1(60),
                time: 5400
            }
        );
        assert_eq!(
            dst.end,
            Rule {
                day: Day::Julian0(300),
                time: -3600
            }
        );
        assert!(PosixTz::parse("<-03>3<-02>,M3.5.0/-2,M10.5.0/-1").is_some());
        assert!(PosixTz::parse("XXX3EDT4,M3.2.0/167,M11.1.0/2").is_some());

        for bad in [
            "",
            "PST",
            "PST8PDT",
            "PST8PDT,M3.2.0",
            "PST8PDT,M13.1.0,M11.1.0",
            "P8",
            "PST25",
        ] {
            assert_eq!(PosixTz::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn julian_one_skips_february_29() {
        let rule = Rule {
            day: Day::Julian1(60),
            time: 0,
        };
        // J60 is March 1 in leap and common years alike.
        assert_eq!(rule.local_seconds(2024), Some(utc(2024, 3, 1, 0, 0)));
        assert_eq!(rule.local_seconds(2025), Some(utc(2025, 3, 1, 0, 0)));
        let rule = Rule {
            day: Day::Julian0(59),
            time: 0,
        };
        assert_eq!(rule.local_seconds(2024), Some(utc(2024, 2, 29, 0, 0)));
        assert_eq!(rule.local_seconds(2025), Some(utc(2025, 3, 1, 0, 0)));
    }

    #[test]
    fn year_round_dst_rule_stays_in_dst() {
        // tzcode's encoding of permanent DST: start Jan 1 00:00, end Dec 31
        // 25:00 (in DST), which lands exactly on next year's start.
        let tz = PosixTz::parse("EST5EDT,0/0,J365/25").unwrap();
        for t in [
            utc(2030, 1, 1, 5, 0),
            utc(2030, 6, 1, 0, 0),
            utc(2030, 12, 31, 23, 0),
        ] {
            assert_eq!(tz.offset_at(t), -4 * 3600);
        }
    }

    #[test]
    fn southern_hemisphere_rule_wraps_the_year() {
        // Sydney: DST from the first Sunday of October to the first Sunday
        // of April.
        let tz = PosixTz::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(tz.offset_at(utc(2030, 1, 15, 0, 0)), 11 * 3600);
        assert_eq!(tz.offset_at(utc(2030, 7, 15, 0, 0)), 10 * 3600);
        assert_eq!(tz.offset_at(utc(2030, 12, 15, 0, 0)), 11 * 3600);
    }

    /// A v2 TZif file with a minimal v1 block (one type, no transitions),
    /// as `zic -b slim` writes, then the given 64-bit data and footer.
    fn slim_tzif(transitions: &[(i64, u8)], offsets: &[i32], footer: &str) -> Vec<u8> {
        fn header(out: &mut Vec<u8>, timecnt: usize, typecnt: usize, charcnt: usize) {
            out.extend_from_slice(b"TZif2");
            out.extend_from_slice(&[0; 15]);
            for count in [0, 0, 0, timecnt, typecnt, charcnt] {
                out.extend_from_slice(&(count as u32).to_be_bytes());
            }
        }
        let mut out = Vec::new();
        header(&mut out, 0, 1, 1);
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0]);
        header(&mut out, transitions.len(), offsets.len(), 1);
        for (t, _) in transitions {
            out.extend_from_slice(&t.to_be_bytes());
        }
        out.extend(transitions.iter().map(|&(_, idx)| idx));
        for o in offsets {
            out.extend_from_slice(&o.to_be_bytes());
            out.extend_from_slice(&[0, 0]);
        }
        out.push(0);
        out.extend_from_slice(format!("\n{footer}\n").as_bytes());
        out
    }

    #[test]
    fn slim_file_defers_to_the_footer_after_its_last_transition() {
        // LMT until 1883, then PST; everything after comes from the footer.
        let bytes = slim_tzif(
            &[(-2_717_640_000, 1)],
            &[-28_378, -28_800],
            "PST8PDT,M3.2.0,M11.1.0",
        );
        let z = Zone::parse_tzif(&bytes).unwrap();
        assert_eq!(z.offset_at(-2_800_000_000), -28_378);
        assert_eq!(z.offset_at(utc(2025, 1, 15, 0, 0)), -8 * 3600);
        assert_eq!(z.offset_at(utc(2025, 7, 15, 0, 0)), -7 * 3600);
        assert_eq!(
            z.utc_from_local(local(2025, 3, 9, 2, 30)),
            LocalResult::None
        );
        assert!(matches!(
            z.utc_from_local(local(2025, 11, 2, 1, 30)),
            LocalResult::Ambiguous(..)
        ));

        // No transitions at all: the footer covers every instant.
        let z = Zone::parse_tzif(&slim_tzif(&[], &[32_400], "JST-9")).unwrap();
        assert_eq!(z.offset_at(utc(1900, 1, 1, 0, 0)), 9 * 3600);
        assert_eq!(
            single(z.utc_from_local(local(2025, 1, 1, 9, 0))),
            utc(2025, 1, 1, 0, 0)
        );

        // An unparseable footer makes the whole zone unknown.
        assert_eq!(Zone::parse_tzif(&slim_tzif(&[], &[0], "garbage")), None);
    }

    #[test]
    fn loads_from_an_alternate_zone_directory() {
        // What `TZDIR` points `load` at; tested through `load_from` so no
        // test mutates the process environment.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Test")).unwrap();
        let bytes = slim_tzif(&[], &[19_800], "<+0530>-5:30");
        std::fs::write(dir.path().join("Test/Zone"), bytes).unwrap();
        let z = Zone::load_from(dir.path(), "Test/Zone").unwrap();
        assert_eq!(z.offset_at(utc(2025, 1, 1, 0, 0)), 19_800);
        assert_eq!(Zone::load_from(dir.path(), "America/New_York"), None);
        assert_eq!(Zone::load_from(dir.path(), "Test"), None);
        // An oversized file is refused before it is parsed.
        std::fs::write(
            dir.path().join("Big"),
            vec![0u8; MAX_FILE_BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(Zone::load_from(dir.path(), "Big"), None);
    }

    #[test]
    fn rejects_truncated_and_foreign_files() {
        assert_eq!(Zone::parse_tzif(b""), None);
        assert_eq!(Zone::parse_tzif(b"not a zone file at all"), None);
        // A v1 header claiming far more transitions than the file holds.
        let mut forged = b"TZif\0".to_vec();
        forged.extend_from_slice(&[0; 15]);
        for count in [0u32, 0, 0, u32::MAX, 1, 0] {
            forged.extend_from_slice(&count.to_be_bytes());
        }
        assert_eq!(Zone::parse_tzif(&forged), None);
        let bytes =
            std::fs::read(std::path::Path::new("/usr/share/zoneinfo").join("America/New_York"))
                .unwrap();
        for len in [10, 44, 60, bytes.len() - 1] {
            assert_eq!(Zone::parse_tzif(&bytes[..len]), None, "truncated at {len}");
        }
        assert!(Zone::parse_tzif(&bytes).is_some());
    }

    /// Every offset change chrono-tz (the crate this module replaced)
    /// reported for these zones from 2000 through 2025, recorded before it
    /// was removed: `<zone> <utc seconds> <offset from then on>`. The first
    /// row per zone is the offset on 2000-01-01. Past-only, so a newer
    /// system tzdata (which revises future rules) still matches.
    const CHRONO_TZ_TRANSITIONS: &str = include_str!("zoneinfo/chrono_tz_transitions.txt");

    #[test]
    fn matches_recorded_chrono_tz_transitions() {
        let mut zones: Vec<(&str, Vec<(i64, i32)>)> = Vec::new();
        for line in CHRONO_TZ_TRANSITIONS.lines() {
            let mut fields = line.split(' ');
            let (name, t, o) = (
                fields.next().unwrap(),
                fields.next().unwrap(),
                fields.next().unwrap(),
            );
            let row = (t.parse().unwrap(), o.parse().unwrap());
            match zones.last_mut() {
                Some((last, rows)) if *last == name => rows.push(row),
                _ => zones.push((name, vec![row])),
            }
        }
        assert_eq!(zones.len(), 17);
        let end = utc(2026, 1, 1, 0, 0);
        for (name, rows) in zones {
            let ours = zone(name);
            for (i, &(at, offset)) in rows.iter().enumerate() {
                assert_eq!(ours.offset_at(at), offset, "{name} at {at}");
                if i > 0 {
                    assert_eq!(
                        ours.offset_at(at - 1),
                        rows[i - 1].1,
                        "{name} just before {at}"
                    );
                }
                // No change of our own between this row and the next.
                let until = rows.get(i + 1).map_or(end, |&(next, _)| next);
                let mut t = at;
                while t < until {
                    assert_eq!(ours.offset_at(t), offset, "{name} at {t}");
                    // Every local reading maps back to the instant it came from.
                    let local = DateTime::from_timestamp(t + offset as i64, 0)
                        .unwrap()
                        .naive_utc();
                    let back = match ours.utc_from_local(local) {
                        LocalResult::Single(a) => vec![a],
                        LocalResult::Ambiguous(a, b) => vec![a, b],
                        LocalResult::None => vec![],
                    };
                    assert!(
                        back.iter().any(|b| b.timestamp() == t),
                        "{name} local {local}"
                    );
                    t += 6 * 3600 + 7;
                }
            }
        }
    }
}
