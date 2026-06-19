//! Cross-platform total-RAM probe + a small heuristic the wizard uses to
//! suggest a sensible worker count.

#[cfg(target_os = "macos")]
use std::process::Command;

const GIB: u64 = 1024 * 1024 * 1024;

/// Total physical RAM in bytes for the machine running this process.
///
/// macOS: `sysctl -n hw.memsize`. Linux: `MemTotal` in `/proc/meminfo`.
/// Other platforms return [`crate::Error::Other`] — the wizard treats that
/// as "skip the recommendation and ask outright."
pub fn total_memory_bytes() -> crate::Result<u64> {
    #[cfg(target_os = "macos")]
    {
        macos_sysctl_memsize()
    }
    #[cfg(target_os = "linux")]
    {
        linux_proc_meminfo_total()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(crate::Error::Other(
            "total_memory_bytes: unsupported platform".into(),
        ))
    }
}

#[cfg(target_os = "macos")]
fn macos_sysctl_memsize() -> crate::Result<u64> {
    let out = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .map_err(crate::Error::Io)?;
    if !out.status.success() {
        return Err(crate::Error::Command {
            cmd: "sysctl -n hw.memsize".into(),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim()
        .parse::<u64>()
        .map_err(|e| crate::Error::Other(format!("parse hw.memsize {s:?}: {e}")))
}

#[cfg(target_os = "linux")]
fn linux_proc_meminfo_total() -> crate::Result<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").map_err(crate::Error::Io)?;
    parse_meminfo_total(&contents)
}

#[cfg(target_os = "linux")]
fn parse_meminfo_total(contents: &str) -> crate::Result<u64> {
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            // Line looks like: "MemTotal:       16380244 kB"
            let mut parts = rest.split_whitespace();
            let value = parts
                .next()
                .ok_or_else(|| crate::Error::Other("MemTotal: missing value".into()))?;
            let unit = parts.next().unwrap_or("kB");
            let n: u64 = value
                .parse()
                .map_err(|e| crate::Error::Other(format!("parse MemTotal {value:?}: {e}")))?;
            let multiplier: u64 = match unit {
                "kB" | "KB" => 1024,
                "B" => 1,
                "mB" | "MB" => 1024 * 1024,
                other => {
                    return Err(crate::Error::Other(format!(
                        "MemTotal: unexpected unit {other:?}"
                    )))
                }
            };
            return Ok(n * multiplier);
        }
    }
    Err(crate::Error::Other(
        "MemTotal not found in /proc/meminfo".into(),
    ))
}

/// Suggest a comfortable number of workers for a single machine.
///
/// Anchored on the observation that one Claude worker holds roughly 2 GB
/// resident, but in practice each worker pulls in editors, language
/// servers, and build processes — so the effective per-worker footprint
/// is closer to 10 GB on the hub (where the user is also working) and a
/// bit more headroom when work is spread across several boxes.
///
/// Clamped to `[1, 16]` so a 2 GB Raspberry Pi still gets one worker and
/// a 512 GB workstation doesn't get a wall-of-claude.
pub fn recommended_worker_count(total_mem_bytes: u64, machine_count: u32) -> u32 {
    let total_gb = (total_mem_bytes / GIB) as f64;
    let per_worker_gb = if machine_count > 1 { 12.0 } else { 10.0 };
    let workers = (total_gb / per_worker_gb).floor() as u32;
    workers.clamp(1, 16)
}

/// Format `bytes` as a short GB/MB string suitable for the wizard prompt
/// (e.g. `"64 GB"`, `"7.7 GB"`, `"512 MB"`).
pub fn format_bytes_short(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb < 1024.0 {
        return format!("{} MB", mb.round() as u64);
    }
    let gb = mb / 1024.0;
    if gb >= 10.0 {
        format!("{} GB", gb.round() as u64)
    } else {
        format!("{gb:.1} GB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommendation_matches_documented_example() {
        // 64 GB hub-only → 6 workers per machine (matches the wizard prompt).
        assert_eq!(recommended_worker_count(64 * GIB, 1), 6);
    }

    #[test]
    fn recommendation_is_clamped() {
        // Tiny boxes still get one worker.
        assert_eq!(recommended_worker_count(2 * GIB, 1), 1);
        assert_eq!(recommended_worker_count(0, 1), 1);
        // Huge boxes don't get a wall-of-claude.
        assert_eq!(recommended_worker_count(1024 * GIB, 1), 16);
    }

    #[test]
    fn recommendation_eases_off_in_multi_machine_projects() {
        // With remotes in the picture, each machine gets a slightly more
        // generous per-worker budget — work can spread.
        let single = recommended_worker_count(64 * GIB, 1);
        let multi = recommended_worker_count(64 * GIB, 3);
        assert!(multi <= single);
    }

    #[test]
    fn formats_human_short() {
        assert_eq!(format_bytes_short(64 * GIB), "64 GB");
        assert_eq!(format_bytes_short(16 * GIB), "16 GB");
        assert_eq!(format_bytes_short(8 * GIB), "8.0 GB");
        assert_eq!(format_bytes_short(512 * 1024 * 1024), "512 MB");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_proc_meminfo_total_line() {
        let sample = "\
MemTotal:       16380244 kB
MemFree:         8000000 kB
Buffers:          400000 kB
";
        let bytes = parse_meminfo_total(sample).unwrap();
        assert_eq!(bytes, 16_380_244u64 * 1024);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn meminfo_missing_total_errors() {
        let sample = "MemFree: 1024 kB\n";
        assert!(parse_meminfo_total(sample).is_err());
    }
}
