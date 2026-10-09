//! Detect that a long-lived process (the hub daemon) has lost its macOS GUI
//! login session.
//!
//! When the user logs out and back in — or the window server restarts after a
//! crash or update — a daemon started in the *old* session keeps running as an
//! orphan. Every process it then spawns inherits a dead per-user bootstrap
//! context: DNS stops resolving ("No DNS configuration available"), user
//! lookups fail ("No user exists for uid 501"), and the launchd `ssh-agent`
//! has moved to a new socket so the stale `SSH_AUTH_SOCK` no longer works. The
//! daemon keeps starting broken agents and nothing notices.
//!
//! ## Signals, and why this combination
//!
//! The **decisive** signal is `launchctl managername`: in a live GUI login
//! session it prints `Aqua`; an orphaned process's query fails (its bootstrap
//! domain is gone). This is a cheap, *local* query — it never depends on the
//! network, so a brief network outage can't flip it. That is exactly the
//! property we need: the acceptance bar is "no false positives on a healthy
//! session (don't treat a brief network outage as session loss)."
//!
//! DNS resolution and the `SSH_AUTH_SOCK` path are only **corroborating**: we
//! record them in the reason string so an operator sees *why* we concluded the
//! session was lost, but neither can trip the verdict on its own. A machine
//! that is briefly offline (DNS fails) but still in a live Aqua session is
//! reported healthy.
//!
//! On top of that, [`SessionHealthTracker`] requires the decisive signal to
//! read "lost" on [`DEFAULT_LOST_THRESHOLD`] *consecutive* checks before it
//! declares the session lost, so a one-off probe hiccup is ridden out.
//!
//! The probe itself is a seam ([`SessionProbe`]): production uses
//! [`RealSessionProbe`] (macOS); the daemon's monitor and the unit tests inject
//! their own readings. On non-macOS platforms the real probe always reports a
//! healthy, in-session reading, so the whole feature is a no-op there.

/// One reading of the process's session context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReading {
    /// `launchctl managername` confirmed we are in the GUI (`Aqua`) login
    /// session. The **decisive** signal: `false` (the query failed, or named a
    /// different manager) means the process is no longer in a live GUI session.
    /// Always `true` on non-macOS platforms (the feature is a no-op there).
    pub manager_aqua: bool,
    /// A system-resolver name lookup succeeded. **Corroborating only** — a
    /// transient network outage flips this without any session loss, so it
    /// never decides the verdict on its own.
    pub dns_ok: bool,
    /// The `SSH_AUTH_SOCK` the process inherited still exists on disk.
    /// **Corroborating only**; `true` when the process never had one to lose.
    pub ssh_auth_sock_ok: bool,
}

impl SessionReading {
    /// A healthy, in-session reading — the value non-macOS builds always report
    /// and a convenient base for tests.
    pub fn healthy() -> Self {
        Self {
            manager_aqua: true,
            dns_ok: true,
            ssh_auth_sock_ok: true,
        }
    }
}

/// The health verdict for a single probe reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickVerdict {
    /// The process is in a live GUI login session.
    Healthy,
    /// The decisive signal says we are no longer in a live GUI session. The
    /// reason records the corroborating detail (DNS / SSH) for disclosure.
    Lost { reason: String },
}

/// Classify a single reading. Only the decisive signal (`manager_aqua`) can
/// make the verdict `Lost`; DNS and SSH only enrich the reason string.
pub fn classify(reading: &SessionReading) -> TickVerdict {
    if reading.manager_aqua {
        return TickVerdict::Healthy;
    }
    let mut parts = vec!["launchctl-managername-not-aqua".to_string()];
    if !reading.dns_ok {
        parts.push("dns-unresolved".to_string());
    }
    if !reading.ssh_auth_sock_ok {
        parts.push("ssh-auth-sock-missing".to_string());
    }
    TickVerdict::Lost {
        reason: parts.join(","),
    }
}

/// Default number of consecutive `Lost` readings before the session is declared
/// lost. Three readings on the monitor's cadence rides out any one-off probe
/// hiccup while still reacting within a few minutes.
pub const DEFAULT_LOST_THRESHOLD: u32 = 3;

/// A transition the monitor should act on, from [`SessionHealthTracker::observe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthTransition {
    /// Nothing to do this tick (steady state, or still debouncing).
    None,
    /// The session has just been declared lost (crossed the threshold).
    BecameLost { reason: String },
    /// A previously-lost session is healthy again.
    Recovered,
}

/// Debounce + edge-detect the per-tick verdicts into [`HealthTransition`]s.
///
/// A `Lost` verdict only becomes a `BecameLost` transition after
/// `threshold` consecutive `Lost` readings, and only once (further losses are
/// `None` until a recovery resets it). A `Healthy` reading resets the counter
/// and, if we had declared loss, yields `Recovered`.
#[derive(Debug, Clone)]
pub struct SessionHealthTracker {
    threshold: u32,
    consecutive_lost: u32,
    declared_lost: bool,
}

impl SessionHealthTracker {
    /// A tracker that declares loss after `threshold` consecutive `Lost`
    /// readings (clamped to at least 1).
    pub fn new(threshold: u32) -> Self {
        Self {
            threshold: threshold.max(1),
            consecutive_lost: 0,
            declared_lost: false,
        }
    }

    /// Seed the tracker as already having declared loss. The daemon's monitor
    /// calls this at startup when a session-lost marker is already on disk, so
    /// the first *healthy* reading yields [`HealthTransition::Recovered`]
    /// (clearing the marker) and a still-lost session is not re-announced.
    pub fn seed_declared_lost(&mut self) {
        self.declared_lost = true;
        self.consecutive_lost = self.threshold;
    }

    /// Whether loss is currently declared (past the threshold, not yet
    /// recovered).
    pub fn is_declared_lost(&self) -> bool {
        self.declared_lost
    }

    /// Fold one verdict into the tracker and report any transition to act on.
    pub fn observe(&mut self, verdict: TickVerdict) -> HealthTransition {
        match verdict {
            TickVerdict::Healthy => {
                self.consecutive_lost = 0;
                if self.declared_lost {
                    self.declared_lost = false;
                    HealthTransition::Recovered
                } else {
                    HealthTransition::None
                }
            }
            TickVerdict::Lost { reason } => {
                self.consecutive_lost = self.consecutive_lost.saturating_add(1);
                if !self.declared_lost && self.consecutive_lost >= self.threshold {
                    self.declared_lost = true;
                    HealthTransition::BecameLost { reason }
                } else {
                    HealthTransition::None
                }
            }
        }
    }
}

/// The probe seam: produce a live [`SessionReading`]. Production uses
/// [`RealSessionProbe`]; tests inject their own.
pub trait SessionProbe {
    fn read(&self) -> SessionReading;
}

/// The real session probe.
///
/// On macOS it shells out to `launchctl managername`, resolves a hostname
/// through the system resolver, and stats the inherited `SSH_AUTH_SOCK`. On
/// every other platform it reports a healthy, in-session reading so the feature
/// is a no-op.
pub struct RealSessionProbe {
    /// A hostname whose resolution stands in for "the system resolver works".
    /// Corroborating only.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    dns_host: String,
    /// The `SSH_AUTH_SOCK` path the process holds, if any.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    ssh_auth_sock: Option<std::path::PathBuf>,
}

impl RealSessionProbe {
    /// Build a probe. `dns_host` is the hostname to resolve for the
    /// corroborating DNS signal; `ssh_auth_sock` is the agent socket path the
    /// process inherited (typically `$SSH_AUTH_SOCK`), or `None` if it had none.
    pub fn new(dns_host: impl Into<String>, ssh_auth_sock: Option<std::path::PathBuf>) -> Self {
        Self {
            dns_host: dns_host.into(),
            ssh_auth_sock,
        }
    }
}

impl SessionProbe for RealSessionProbe {
    #[cfg(target_os = "macos")]
    fn read(&self) -> SessionReading {
        SessionReading {
            manager_aqua: macos_manager_is_aqua(),
            dns_ok: resolver_resolves(&self.dns_host),
            ssh_auth_sock_ok: ssh_auth_sock_ok(self.ssh_auth_sock.as_deref()),
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn read(&self) -> SessionReading {
        // Not a macOS GUI session concern on other platforms: always healthy.
        SessionReading::healthy()
    }
}

/// Whether `launchctl managername` reports `Aqua` (we are in a live GUI login
/// session). Any failure — a non-zero exit, a spawn error, or a different name
/// — is treated as "not in a live GUI session". A local query that never
/// touches the network, so a transient network outage can't flip it.
#[cfg(target_os = "macos")]
fn macos_manager_is_aqua() -> bool {
    match std::process::Command::new("launchctl")
        .arg("managername")
        .output()
    {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim() == "Aqua"
        }
        _ => false,
    }
}

/// Whether the system resolver can resolve `host`. Corroborating only. A
/// config-level failure ("No DNS configuration available") returns promptly;
/// a genuinely-offline-but-healthy machine may block for the resolver's own
/// timeout, but since this never decides the verdict that is harmless.
#[cfg(target_os = "macos")]
fn resolver_resolves(host: &str) -> bool {
    use std::net::ToSocketAddrs;
    // Port 0 is irrelevant — we only need name resolution to run.
    (host, 0u16)
        .to_socket_addrs()
        .map(|mut addrs| addrs.next().is_some())
        .unwrap_or(false)
}

/// Whether the inherited `SSH_AUTH_SOCK` still exists. `None` (no socket to
/// lose) counts as fine.
#[cfg(target_os = "macos")]
fn ssh_auth_sock_ok(sock: Option<&std::path::Path>) -> bool {
    match sock {
        Some(p) => p.exists(),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_healthy_when_manager_is_aqua_even_if_dns_down() {
        // A brief network outage flips dns_ok but must NOT be read as a session
        // loss while we are still in the GUI (Aqua) session.
        let reading = SessionReading {
            manager_aqua: true,
            dns_ok: false,
            ssh_auth_sock_ok: false,
        };
        assert_eq!(classify(&reading), TickVerdict::Healthy);
    }

    #[test]
    fn classify_lost_names_the_corroborating_detail() {
        let reading = SessionReading {
            manager_aqua: false,
            dns_ok: false,
            ssh_auth_sock_ok: false,
        };
        match classify(&reading) {
            TickVerdict::Lost { reason } => {
                assert!(reason.contains("launchctl-managername-not-aqua"), "{reason}");
                assert!(reason.contains("dns-unresolved"), "{reason}");
                assert!(reason.contains("ssh-auth-sock-missing"), "{reason}");
            }
            other => panic!("expected Lost, got {other:?}"),
        }
    }

    #[test]
    fn classify_lost_decisive_signal_alone_is_enough() {
        // managername failed but DNS/SSH are fine: still Lost (decisive signal),
        // and the reason carries only the decisive token.
        let reading = SessionReading {
            manager_aqua: false,
            dns_ok: true,
            ssh_auth_sock_ok: true,
        };
        assert_eq!(
            classify(&reading),
            TickVerdict::Lost {
                reason: "launchctl-managername-not-aqua".to_string()
            }
        );
    }

    #[test]
    fn tracker_debounces_a_transient_blip_below_threshold() {
        let mut t = SessionHealthTracker::new(3);
        let lost = || TickVerdict::Lost {
            reason: "x".into(),
        };
        // Two losses then a recovery: never crosses the threshold, so no
        // BecameLost and no spurious Recovered.
        assert_eq!(t.observe(lost()), HealthTransition::None);
        assert_eq!(t.observe(lost()), HealthTransition::None);
        assert_eq!(t.observe(TickVerdict::Healthy), HealthTransition::None);
        assert!(!t.is_declared_lost());
    }

    #[test]
    fn tracker_declares_lost_after_threshold_then_recovers_once() {
        let mut t = SessionHealthTracker::new(3);
        let lost = || TickVerdict::Lost {
            reason: "launchctl-managername-not-aqua".into(),
        };
        assert_eq!(t.observe(lost()), HealthTransition::None);
        assert_eq!(t.observe(lost()), HealthTransition::None);
        assert_eq!(
            t.observe(lost()),
            HealthTransition::BecameLost {
                reason: "launchctl-managername-not-aqua".into()
            }
        );
        // Further losses while already declared are silent.
        assert_eq!(t.observe(lost()), HealthTransition::None);
        assert!(t.is_declared_lost());
        // Recovery fires exactly once.
        assert_eq!(t.observe(TickVerdict::Healthy), HealthTransition::Recovered);
        assert_eq!(t.observe(TickVerdict::Healthy), HealthTransition::None);
        assert!(!t.is_declared_lost());
    }

    #[test]
    fn seed_declared_lost_recovers_on_first_healthy_reading() {
        // The daemon-startup case: a marker already on disk seeds the tracker so
        // a healthy first reading clears it, and a still-lost one is not
        // re-announced.
        let mut t = SessionHealthTracker::new(3);
        t.seed_declared_lost();
        assert!(t.is_declared_lost());
        assert_eq!(
            t.observe(TickVerdict::Lost { reason: "x".into() }),
            HealthTransition::None,
            "a still-lost session must not re-announce BecameLost"
        );
        assert_eq!(t.observe(TickVerdict::Healthy), HealthTransition::Recovered);
    }

    #[test]
    fn non_macos_real_probe_always_reports_healthy() {
        // On non-macOS platforms the real probe is a no-op; on macOS this hits
        // the live `launchctl`, so only assert the invariant off-macOS.
        #[cfg(not(target_os = "macos"))]
        {
            let probe = RealSessionProbe::new("example.invalid", None);
            assert_eq!(classify(&probe.read()), TickVerdict::Healthy);
        }
    }
}
