//! Human durations for the awareness CLI and workflow config: `30s`, `5m`,
//! `1h`, `1d`, and compounds like `1h30m`, `2d12h` or `2h5m10s`.
//!
//! The units are `d` (24 hours), `h`, `m` and `s`, any case, in descending
//! order, each at most once. A bare number is rejected: guessing a unit is
//! exactly the foot-gun the `heartbeat` parser avoids, and the two must read
//! alike.
//!
//! Two printers:
//!
//! - [`format_duration`] is the on-disk form ([`serde_compact`], used by the
//!   reminder and job stores) and never writes `d`: `86400s` → `24h`. An older
//!   binary, whose parser knows only `h`, `m` and `s`, has to keep reading a
//!   store a newer one wrote. Don't teach it days.
//! - [`display_duration`] is for people: whole days come out as `Nd`
//!   (`86400s` → `1d`, `2d19h`). Never persist its output.
//!
//! Both round-trip through [`parse_duration`].

use std::time::Duration;

/// Parse a duration like `45s`, `10m`, `2h`, `1h30m`, `1d` or `2d12h`. Zero
/// is rejected.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("empty duration: use a form like `30s`, `5m`, `1h`, `1h30m` or `1d`".into());
    }
    let mut total: u64 = 0;
    let mut digits = String::new();
    // Rank of the last unit seen (d=4, h=3, m=2, s=1); units must strictly
    // descend.
    let mut last_rank = u8::MAX;
    for c in trimmed.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let (mult, rank) = match c.to_ascii_lowercase() {
            'd' => (86_400u64, 4u8),
            'h' => (3_600, 3),
            'm' => (60, 2),
            's' => (1, 1),
            _ => {
                return Err(format!(
                    "duration `{s}`: unexpected `{c}`; use `d`, `h`, `m` or `s` (e.g. `45m`, `1h30m`, `1d`)"
                ))
            }
        };
        if digits.is_empty() {
            return Err(format!("duration `{s}`: `{c}` needs a number before it"));
        }
        if rank >= last_rank {
            return Err(format!(
                "duration `{s}`: units must run largest to smallest, each once (e.g. `1h30m`)"
            ));
        }
        last_rank = rank;
        let n: u64 = digits
            .parse()
            .map_err(|_| format!("duration `{s}`: number out of range"))?;
        digits.clear();
        total = n
            .checked_mul(mult)
            .and_then(|v| total.checked_add(v))
            .ok_or_else(|| format!("duration `{s}`: overflows"))?;
    }
    if !digits.is_empty() {
        return Err(format!(
            "duration `{s}`: missing unit; add `d`, `h`, `m` or `s` (e.g. `{digits}m`)"
        ));
    }
    if total == 0 {
        return Err(format!("duration `{s}`: must be longer than zero"));
    }
    Ok(Duration::from_secs(total))
}

/// Compact form of `d`, whole seconds: `45s`, `10m`, `1h`, `1h10m`, `2h0m5s`
/// is printed `2h5s`. Zero prints `0s`.
///
/// This is the serialization form ([`serde_compact`]), so it stays in `h`,
/// `m` and `s` however long `d` is (`1d` prints `24h`): older parsers don't
/// know `d`. For text a person reads, use [`display_duration`].
pub fn format_duration(d: Duration) -> String {
    compact(d.as_secs(), false)
}

/// [`format_duration`] for display: whole days lead as `Nd` (`1d`, `2d19h`,
/// `1d30m`), the rest as `format_duration` prints it. Display only; persist
/// [`format_duration`].
pub fn display_duration(d: Duration) -> String {
    compact(d.as_secs(), true)
}

fn compact(secs: u64, days: bool) -> String {
    if secs == 0 {
        return "0s".into();
    }
    let (dd, rest) = if days { (secs / 86_400, secs % 86_400) } else { (0, secs) };
    let (h, m, s) = (rest / 3_600, (rest % 3_600) / 60, rest % 60);
    let mut out = String::new();
    if dd > 0 {
        out.push_str(&format!("{dd}d"));
    }
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if s > 0 {
        out.push_str(&format!("{s}s"));
    }
    out
}

/// Serde adapter storing a [`Duration`] as its [`format_duration`] string, so
/// state files read `"estimate": "45m"` rather than a bare seconds count.
pub mod serde_compact {
    use super::{format_duration, parse_duration};
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format_duration(*d))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let raw = String::deserialize(d)?;
        parse_duration(&raw).map_err(serde::de::Error::custom)
    }

    /// The same adapter for an `Option<Duration>` field.
    pub mod option {
        use super::*;

        pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
            match d {
                Some(d) => s.serialize_str(&format_duration(*d)),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
            let raw = Option::<String>::deserialize(d)?;
            raw.map(|r| parse_duration(&r).map_err(serde::de::Error::custom))
                .transpose()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: &str) -> u64 {
        parse_duration(s).unwrap().as_secs()
    }

    #[test]
    fn parses_the_heartbeat_forms_and_compounds() {
        assert_eq!(secs("30s"), 30);
        assert_eq!(secs("5m"), 300);
        assert_eq!(secs("1h"), 3_600);
        assert_eq!(secs("1h30m"), 5_400);
        assert_eq!(secs("2h5m10s"), 7_510);
        assert_eq!(secs(" 45M "), 2_700);
        assert_eq!(secs("90m"), 5_400);
    }

    #[test]
    fn rejects_ambiguous_or_empty_input() {
        for bad in ["", "45", "0m", "1x", "m", "30m1h", "1h1h", "1.5h", "-5m", "1h 30m", "0d", "d"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn parses_days_alone_and_leading_compounds() {
        assert_eq!(secs("1d"), 86_400);
        assert_eq!(secs("3d"), 3 * 86_400);
        assert_eq!(secs("2d12h"), 2 * 86_400 + 12 * 3_600);
        assert_eq!(secs("1d30m"), 86_400 + 1_800);
        assert_eq!(secs("1d1h1m1s"), 86_400 + 3_661);
        assert_eq!(secs("1D"), 86_400);
    }

    #[test]
    fn days_follow_the_ordering_and_repeat_rules() {
        for bad in ["1h1d", "30m1d", "1d1d", "1d2d"] {
            let err = parse_duration(bad).unwrap_err();
            assert!(err.contains("largest to smallest"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn errors_list_the_day_unit() {
        let err = parse_duration("1w").unwrap_err();
        assert!(err.contains("use `d`, `h`, `m` or `s` (e.g. `45m`, `1h30m`, `1d`)"), "{err}");
        let err = parse_duration("45").unwrap_err();
        assert!(err.contains("add `d`, `h`, `m` or `s`"), "{err}");
    }

    #[test]
    fn serialization_never_writes_days() {
        // What an older binary's parser accepts: `h`, `m`, `s` only.
        fn old_parser_reads(text: &str) -> bool {
            !text.is_empty() && text.chars().all(|c| c.is_ascii_digit() || "hms".contains(c))
        }
        for (text, stored) in [("1d", "24h"), ("3d", "72h"), ("2d12h", "60h"), ("1d30m", "24h30m")] {
            let d = parse_duration(text).unwrap();
            assert_eq!(format_duration(d), stored);
            assert!(old_parser_reads(&format_duration(d)));
            assert_eq!(parse_duration(&format_duration(d)).unwrap(), d);
        }

        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Stored {
            #[serde(with = "serde_compact")]
            every: Duration,
            #[serde(with = "serde_compact::option")]
            estimate: Option<Duration>,
        }
        let v = Stored { every: parse_duration("1d").unwrap(), estimate: Some(parse_duration("2d12h").unwrap()) };
        let text = yaml_serde::to_string(&v).unwrap();
        assert_eq!(text, "every: 24h\nestimate: 60h\n");
        assert_eq!(yaml_serde::from_str::<Stored>(&text).unwrap(), v);
    }

    #[test]
    fn display_shows_whole_days() {
        for (secs, text) in [
            (0, "0s"),
            (45, "45s"),
            (3_600, "1h"),
            (23 * 3_600, "23h"),
            (86_400, "1d"),
            (2 * 86_400 + 19 * 3_600, "2d19h"),
            (86_400 + 1_800, "1d30m"),
            (86_400 + 5, "1d5s"),
        ] {
            assert_eq!(display_duration(Duration::from_secs(secs)), text);
            if secs > 0 {
                assert_eq!(parse_duration(text).unwrap().as_secs(), secs);
            }
        }
    }

    #[test]
    fn formats_compactly_and_round_trips() {
        for (secs, text) in [
            (0, "0s"),
            (45, "45s"),
            (600, "10m"),
            (3_600, "1h"),
            (4_200, "1h10m"),
            (7_205, "2h5s"),
            (5_400, "1h30m"),
        ] {
            assert_eq!(format_duration(Duration::from_secs(secs)), text);
            if secs > 0 {
                assert_eq!(parse_duration(text).unwrap().as_secs(), secs);
            }
        }
    }
}
