//! Human durations for the awareness CLI and workflow config: `30s`, `5m`,
//! `1h`, and compounds like `1h30m` or `2h5m10s`.
//!
//! The same unit letters the `heartbeat` config accepts (`s`, `m`, `h`, any
//! case), in descending order, each at most once. A bare number is rejected:
//! guessing a unit is exactly the foot-gun the heartbeat parser avoids, and the
//! two must read alike. [`format_duration`] prints the compact form back
//! (`4200s` → `1h10m`), so a parsed value round-trips.

use std::time::Duration;

/// Parse a duration like `45s`, `10m`, `2h` or `1h30m`. Zero is rejected.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("empty duration: use a form like `30s`, `5m`, `1h` or `1h30m`".into());
    }
    let mut total: u64 = 0;
    let mut digits = String::new();
    // Rank of the last unit seen (h=3, m=2, s=1); units must strictly descend.
    let mut last_rank = u8::MAX;
    for c in trimmed.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let (mult, rank) = match c.to_ascii_lowercase() {
            'h' => (3_600u64, 3u8),
            'm' => (60, 2),
            's' => (1, 1),
            _ => {
                return Err(format!(
                    "duration `{s}`: unexpected `{c}`; use `s`, `m` or `h` (e.g. `45m`, `1h30m`)"
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
            "duration `{s}`: missing unit; add `s`, `m` or `h` (e.g. `{digits}m`)"
        ));
    }
    if total == 0 {
        return Err(format!("duration `{s}`: must be longer than zero"));
    }
    Ok(Duration::from_secs(total))
}

/// Compact form of `d`, whole seconds: `45s`, `10m`, `1h`, `1h10m`, `2h0m5s`
/// is printed `2h5s`. Zero prints `0s`.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs == 0 {
        return "0s".into();
    }
    let (h, m, s) = (secs / 3_600, (secs % 3_600) / 60, secs % 60);
    let mut out = String::new();
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
        for bad in ["", "45", "0m", "1x", "m", "30m1h", "1h1h", "1.5h", "-5m", "1h 30m"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should be rejected");
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
