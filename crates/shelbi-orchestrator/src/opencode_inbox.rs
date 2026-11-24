//! Verified sends into an OpenCode pane, through Shelbi's plugin instead of
//! keystrokes.
//!
//! `shelbi send` (and every caller of [`crate::agent_send`]) drops one file
//! per message into the pane's worktree:
//!
//! ```text
//! .shelbi/opencode/inbox/<delivery-id>.json   {"id": "<delivery-id>", "text": "…"}
//! ```
//!
//! The plugin (`opencode_plugin.js`) claims the file by renaming it to
//! `<delivery-id>.taken`, admits the text into the pane's session with
//! `ctx.session.prompt({id: "msg_<delivery-id>"})`, and answers with
//!
//! ```text
//! .shelbi/opencode/ack/<delivery-id>.json     {"id": …, "status": "admitted" | "duplicate" | "failed", "error"?: …}
//! ```
//!
//! Nothing is typed into the TUI, so a draft the user has in the composer is
//! left alone, and OpenCode's prompt-id idempotency (a retried id returns the
//! original admission) plus the plugin's own record of admitted ids mean a
//! delivery id is admitted once however often its file is dropped.
//!
//! When no ack comes in time, Shelbi takes the file back (`rm`; the plugin's
//! claim is a rename of the same name, so exactly one side wins) and the
//! caller falls back to a paste. A file the plugin claimed but never acked
//! can't be taken back, and pasting it as well could deliver it twice, so
//! that one is reported as unconfirmed.
//!
//! Delivery ids have the shape of OpenCode's own message ids (12 hex digits
//! of `millis * 4096 + counter`, then 14 base62 characters), so a message
//! Shelbi sends sorts where its send time puts it among the session's
//! messages.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use shelbi_core::{Error, Host, Result};

/// Worktree-relative directory the plugin reads deliveries from.
pub const OPENCODE_INBOX_REL: &str = ".shelbi/opencode/inbox";
/// Worktree-relative directory the plugin writes its acks to.
pub const OPENCODE_ACK_REL: &str = ".shelbi/opencode/ack";

/// After a timed-out wait finds the file already claimed, how much longer to
/// wait for the plugin to finish admitting it.
const CLAIMED_GRACE: Duration = Duration::from_secs(5);

/// How often a local wait looks for the ack.
const LOCAL_POLL: Duration = Duration::from_millis(100);

/// One message's way into an OpenCode pane's inbox.
#[derive(Debug, Clone)]
pub struct InboxTarget {
    pub host: Host,
    /// The pane's worktree (its launch directory).
    pub worktree: PathBuf,
}

/// What became of a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxOutcome {
    /// The plugin admitted the text into the session. `duplicate` when the id
    /// had been admitted before, so nothing new went in.
    Admitted { duplicate: bool },
    /// The plugin took the file but couldn't admit it (`reason` is its
    /// error). Nothing went into the session.
    Rejected { reason: String },
    /// No ack in time and the file was taken back unread: the plugin isn't
    /// reading the inbox.
    Silent,
    /// The plugin claimed the file and never acked it. It may still land, so
    /// a fallback paste could deliver it twice.
    ClaimedUnacked,
    /// The file went in, but reading the answer (or taking the file back)
    /// failed, so whether it landed isn't known. Not pasted either, for the
    /// same reason.
    Unknown { reason: String },
}

/// The plugin's answer, as written to `ack/<id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
struct Ack {
    #[serde(default)]
    status: String,
    #[serde(default)]
    error: Option<String>,
}

impl Ack {
    fn outcome(&self) -> InboxOutcome {
        match self.status.as_str() {
            "admitted" => InboxOutcome::Admitted { duplicate: false },
            "duplicate" => InboxOutcome::Admitted { duplicate: true },
            other => InboxOutcome::Rejected {
                reason: self
                    .error
                    .clone()
                    .filter(|e| !e.trim().is_empty())
                    .unwrap_or_else(|| format!("ack status `{other}`")),
            },
        }
    }
}

/// A fresh delivery id, in the shape of OpenCode's ascending message ids.
pub fn new_delivery_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let seed = (now.as_nanos() as u64) ^ ((std::process::id() as u64) << 32) ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    delivery_id_at(now.as_millis() as u64, n, seed)
}

fn delivery_id_at(millis: u64, counter: u64, seed: u64) -> String {
    const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let stamp = (millis.wrapping_mul(0x1000).wrapping_add(counter % 0x1000)) & 0xffff_ffff_ffff;
    let mut out = format!("{stamp:012x}");
    // splitmix64: 14 well-mixed base62 characters from the seed.
    let mut state = seed;
    for _ in 0..14 {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        out.push(BASE62[(z % 62) as usize] as char);
    }
    out
}

/// Whether `id` is a delivery id the plugin will accept (and so safe as a
/// file name on either side).
pub fn valid_delivery_id(id: &str) -> bool {
    id.len() == 26
        && id[..12].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && id[12..].bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The inbox file's body.
fn entry_json(id: &str, text: &str) -> String {
    let mut body = serde_json::json!({ "id": id, "text": text }).to_string();
    body.push('\n');
    body
}

/// Drop `text` into the inbox as `id` and wait up to `wait` for the plugin's
/// ack. See the module docs for each outcome.
pub fn deliver(target: &InboxTarget, id: &str, text: &str, wait: Duration) -> Result<InboxOutcome> {
    if !valid_delivery_id(id) {
        return Err(Error::Other(format!("`{id}` isn't a valid OpenCode delivery id")));
    }
    deliver_with(
        || post(target, id, text),
        |w| wait_for_ack(target, id, w),
        || withdraw(target, id),
        wait,
    )
}

/// The protocol, with its three operations injected so the decisions are
/// testable without a plugin: `wait_ack` returns the parsed ack, or `None`
/// when none came within the given time; `withdraw` returns whether the file
/// was still there to take back. Only a failed `post` is an error: once the
/// file is in, a failure is an [`InboxOutcome::Unknown`], which the caller
/// must not paste over.
fn deliver_with(
    post: impl FnOnce() -> Result<()>,
    mut wait_ack: impl FnMut(Duration) -> Result<Option<Ack>>,
    withdraw: impl FnOnce() -> Result<bool>,
    wait: Duration,
) -> Result<InboxOutcome> {
    post()?;
    let unknown = |e: Error| InboxOutcome::Unknown { reason: e.to_string() };
    match wait_ack(wait) {
        Ok(Some(ack)) => return Ok(ack.outcome()),
        Ok(None) => {}
        Err(e) => return Ok(unknown(e)),
    }
    match withdraw() {
        Ok(true) => return Ok(InboxOutcome::Silent),
        Ok(false) => {}
        Err(e) => return Ok(unknown(e)),
    }
    // The plugin took it between the deadline and the withdrawal, or is
    // still admitting it: give it a little longer before calling it lost.
    Ok(match wait_ack(CLAIMED_GRACE) {
        Ok(Some(ack)) => ack.outcome(),
        Ok(None) => InboxOutcome::ClaimedUnacked,
        Err(e) => unknown(e),
    })
}

fn inbox_file(worktree: &Path, id: &str) -> PathBuf {
    worktree.join(OPENCODE_INBOX_REL).join(format!("{id}.json"))
}

fn ack_file(worktree: &Path, id: &str) -> PathBuf {
    worktree.join(OPENCODE_ACK_REL).join(format!("{id}.json"))
}

/// Write the entry under a dot-name the plugin skips, then rename it into
/// place, so the plugin never reads half a file.
fn post(target: &InboxTarget, id: &str, text: &str) -> Result<()> {
    let body = entry_json(id, text);
    let dir = target.worktree.join(OPENCODE_INBOX_REL);
    let tmp = dir.join(format!(".{id}.tmp"));
    let dest = inbox_file(&target.worktree, id);
    match &target.host {
        Host::Local => {
            std::fs::create_dir_all(&dir).map_err(Error::Io)?;
            std::fs::write(&tmp, body).map_err(Error::Io)?;
            std::fs::rename(&tmp, &dest).map_err(Error::Io)
        }
        Host::Ssh { .. } => {
            let esc = |p: &Path| shelbi_core::shell_escape(&p.to_string_lossy());
            let script = format!(
                "mkdir -p {dir} && cat > {tmp} && mv -f {tmp} {dest}",
                dir = esc(&dir),
                tmp = esc(&tmp),
                dest = esc(&dest),
            );
            shelbi_ssh::run_with_stdin(&target.host, ["sh", "-c", &script], body.as_bytes()).map(|_| ())
        }
    }
}

/// Wait up to `wait` for `ack/<id>.json`, consume it, and parse it. An ack
/// that can't be parsed still proves the plugin read the entry, so it reads
/// as a rejection rather than as silence.
fn wait_for_ack(target: &InboxTarget, id: &str, wait: Duration) -> Result<Option<Ack>> {
    let file = ack_file(&target.worktree, id);
    let body = match &target.host {
        Host::Local => {
            let deadline = Instant::now() + wait;
            loop {
                match std::fs::read_to_string(&file) {
                    Ok(body) => {
                        let _ = std::fs::remove_file(&file);
                        break Some(body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::Io(e)),
                }
                if Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(LOCAL_POLL);
            }
        }
        // One SSH round trip for the whole wait: the loop runs on the host.
        Host::Ssh { .. } => {
            let tenths = (wait.as_millis() / 200).max(1);
            let f = shelbi_core::shell_escape(&file.to_string_lossy());
            let script = format!(
                "i=0; while [ \"$i\" -lt {tenths} ]; do \
                   if [ -f {f} ]; then cat {f}; rm -f {f}; exit 0; fi; \
                   sleep 0.2; i=$((i+1)); \
                 done; \
                 if [ -f {f} ]; then cat {f}; rm -f {f}; exit 0; fi; exit 3"
            );
            let out = shelbi_ssh::run_with_deadline(
                &target.host,
                ["sh", "-c", &script],
                wait + Duration::from_secs(15),
            )
            .map_err(Error::Io)?;
            match out.status.code() {
                Some(0) => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
                Some(3) => None,
                _ => {
                    return Err(Error::Other(format!(
                        "waiting for the OpenCode plugin's ack: {}",
                        shelbi_ssh::describe_failure(&target.host, &out),
                    )))
                }
            }
        }
    };
    Ok(body.map(|body| {
        serde_json::from_str::<Ack>(body.trim()).unwrap_or_else(|e| Ack {
            status: "unreadable".to_string(),
            error: Some(format!("unreadable ack: {e}")),
        })
    }))
}

/// Take the entry back. `true` when it was still in the inbox (so the
/// plugin never saw it); `false` when the plugin had already claimed it.
fn withdraw(target: &InboxTarget, id: &str) -> Result<bool> {
    let file = inbox_file(&target.worktree, id);
    match &target.host {
        Host::Local => match std::fs::remove_file(&file) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Error::Io(e)),
        },
        Host::Ssh { .. } => {
            let f = shelbi_core::shell_escape(&file.to_string_lossy());
            let script = format!("if rm {f} 2>/dev/null; then echo withdrawn; else echo taken; fi");
            let out = shelbi_ssh::run_capture(&target.host, ["sh", "-c", &script])?;
            Ok(out.trim() == "withdrawn")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn ack(status: &str) -> Ack {
        Ack { status: status.into(), error: None }
    }

    #[test]
    fn delivery_ids_have_opencodes_ascending_message_id_shape() {
        let a = delivery_id_at(1_700_000_000_000, 1, 7);
        let b = delivery_id_at(1_700_000_000_000, 2, 7);
        let c = delivery_id_at(1_700_000_000_001, 0, 7);
        for id in [&a, &b, &c] {
            assert!(valid_delivery_id(id), "{id}");
        }
        // Sorted by send time, the way OpenCode orders its own ids.
        assert!(a < b && b < c, "{a} {b} {c}");
        // OpenCode keeps the low 48 bits of `millis * 4096 + counter`.
        assert_eq!(&a[..12], format!("{:012x}", (1_700_000_000_000u64 * 4096 + 1) & 0xffff_ffff_ffff));
        // Fresh ids differ even within one millisecond.
        assert_ne!(new_delivery_id(), new_delivery_id());
        assert!(valid_delivery_id(&new_delivery_id()));
        // What the plugin and the file names rely on.
        assert!(!valid_delivery_id("../../etc/passwd"));
        assert!(!valid_delivery_id(&format!("{a}x")));
        assert!(!valid_delivery_id(&a.replace(&a[..1], "G")));
    }

    #[test]
    fn an_ack_decides_the_outcome() {
        let outcome = deliver_with(|| Ok(()), |_| Ok(Some(ack("admitted"))), || panic!("no withdraw"), Duration::ZERO);
        assert_eq!(outcome.unwrap(), InboxOutcome::Admitted { duplicate: false });
        let outcome = deliver_with(|| Ok(()), |_| Ok(Some(ack("duplicate"))), || panic!("no withdraw"), Duration::ZERO);
        assert_eq!(outcome.unwrap(), InboxOutcome::Admitted { duplicate: true });
        let failed = Ack { status: "failed".into(), error: Some("Session not found".into()) };
        let outcome = deliver_with(|| Ok(()), |_| Ok(Some(failed.clone())), || panic!("no withdraw"), Duration::ZERO);
        assert_eq!(outcome.unwrap(), InboxOutcome::Rejected { reason: "Session not found".into() });
    }

    #[test]
    fn silence_takes_the_file_back_and_a_claimed_file_gets_a_grace() {
        // Never claimed: withdrawn, silent.
        let outcome = deliver_with(|| Ok(()), |_| Ok(None), || Ok(true), Duration::ZERO);
        assert_eq!(outcome.unwrap(), InboxOutcome::Silent);

        // Claimed at the deadline and acked within the grace.
        let waits = RefCell::new(Vec::new());
        let outcome = deliver_with(
            || Ok(()),
            |w| {
                waits.borrow_mut().push(w);
                Ok((waits.borrow().len() == 2).then(|| ack("admitted")))
            },
            || Ok(false),
            Duration::from_secs(15),
        );
        assert_eq!(outcome.unwrap(), InboxOutcome::Admitted { duplicate: false });
        assert_eq!(*waits.borrow(), vec![Duration::from_secs(15), CLAIMED_GRACE]);

        // Claimed and never acked: not pasted, reported.
        let outcome = deliver_with(|| Ok(()), |_| Ok(None), || Ok(false), Duration::ZERO);
        assert_eq!(outcome.unwrap(), InboxOutcome::ClaimedUnacked);
    }

    #[test]
    fn a_failure_after_the_post_is_unknown_never_a_paste() {
        let outcome = deliver_with(|| Ok(()), |_| Err(Error::Other("ssh down".into())), || panic!("no withdraw"), Duration::ZERO);
        assert!(matches!(outcome.unwrap(), InboxOutcome::Unknown { reason } if reason.contains("ssh down")));
        let outcome = deliver_with(|| Ok(()), |_| Ok(None), || Err(Error::Other("ssh down".into())), Duration::ZERO);
        assert!(matches!(outcome.unwrap(), InboxOutcome::Unknown { .. }));
    }

    #[test]
    fn a_failed_post_is_an_error_not_an_outcome() {
        let outcome = deliver_with(
            || Err(Error::Other("disk full".into())),
            |_| panic!("no wait"),
            || panic!("no withdraw"),
            Duration::ZERO,
        );
        assert!(outcome.is_err());
    }

    #[test]
    fn local_post_wait_and_withdraw_round_trip() {
        let wt = std::env::temp_dir().join(format!("shelbi-inbox-{}-{}", std::process::id(), new_delivery_id()));
        let target = InboxTarget { host: Host::Local, worktree: wt.clone() };
        let id = new_delivery_id();

        // No reader: the entry is taken back.
        let outcome = deliver(&target, &id, "hello \"there\"\nsecond line", Duration::ZERO).unwrap();
        assert_eq!(outcome, InboxOutcome::Silent);
        assert!(!inbox_file(&wt, &id).exists());

        // The entry is whole JSON, and an ack written for it is consumed.
        post(&target, &id, "hi").unwrap();
        let entry: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(inbox_file(&wt, &id)).unwrap()).unwrap();
        assert_eq!(entry["id"], id.as_str());
        assert_eq!(entry["text"], "hi");
        std::fs::create_dir_all(wt.join(OPENCODE_ACK_REL)).unwrap();
        std::fs::write(ack_file(&wt, &id), r#"{"id":"x","status":"admitted"}"#).unwrap();
        assert_eq!(wait_for_ack(&target, &id, Duration::ZERO).unwrap(), Some(ack("admitted")));
        assert!(!ack_file(&wt, &id).exists());

        // A garbled ack is a rejection, not silence.
        std::fs::write(ack_file(&wt, &id), "{oops").unwrap();
        let read = wait_for_ack(&target, &id, Duration::ZERO).unwrap().unwrap();
        assert!(matches!(read.outcome(), InboxOutcome::Rejected { .. }));

        // A claimed (renamed) entry can't be withdrawn.
        std::fs::rename(inbox_file(&wt, &id), wt.join(OPENCODE_INBOX_REL).join(format!("{id}.taken"))).unwrap();
        assert!(!withdraw(&target, &id).unwrap());
        assert!(deliver(&target, "nope", "x", Duration::ZERO).is_err());
        let _ = std::fs::remove_dir_all(&wt);
    }

    /// The Rust sender against the real plugin, run under Node with a fake
    /// OpenCode `api`: a send is admitted into the pane's own session under
    /// `msg_<delivery-id>` and confirmed; the same delivery id sent again is
    /// confirmed without a second prompt; a prompt OpenCode refuses falls
    /// back to a paste and says why; with the plugin gone, the file is taken
    /// back and pasted. Skipped without `node`.
    #[test]
    fn sends_are_admitted_once_through_the_plugin_or_pasted() {
        use crate::submit::{send_through_opencode_inbox_with, SubmitStatus};
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        if Command::new("node").arg("--version").output().is_err() {
            eprintln!("node not found; skipping the plugin inbox test");
            return;
        }
        let root = std::env::temp_dir().join(format!("shelbi-inbox-node-{}-{}", std::process::id(), new_delivery_id()));
        let wt = root.join("wt");
        std::fs::create_dir_all(wt.join(".shelbi/hooks")).unwrap();
        let plugin = root.join("plugin.mjs");
        std::fs::write(&plugin, crate::workspace::opencode_plugin_js()).unwrap();
        let prompts = root.join("prompts.jsonl");
        let script = root.join("run.mjs");
        std::fs::write(
            &script,
            r#"
import { appendFileSync } from "node:fs";
const [pluginPath, wt, log] = process.argv.slice(2);
const plugin = (await import(pluginPath)).default;
const api = {
  location: { directory: wt },
  session: {
    get: async () => ({}),
    update: async () => {},
    synthetic: async () => {},
    prompt: async (input) => {
      if (input.text === "boom") throw new Error("Session.NotFoundError");
      appendFileSync(log, JSON.stringify(input) + "\n");
      return { id: input.id };
    },
  },
  event: { subscribe: (opts) => ({ async *[Symbol.asyncIterator]() {
    await new Promise((r) => opts.signal.addEventListener("abort", r));
  } }) },
};
const stop = await plugin.setup(api);
console.log(typeof stop === "function" ? "ready" : "passive");
process.stdin.resume();
process.stdin.on("end", () => { stop?.(); process.exit(0); });
"#,
        )
        .unwrap();
        let mut node = Command::new("node")
            .arg(&script)
            .arg(&plugin)
            .arg(&wt)
            .arg(&prompts)
            // Never report to the live hub this test may run under.
            .env_remove("SHELBI_HUB_ADDR")
            .env_remove("SHELBI_HUB_SOCK")
            .env_remove("PROJECT")
            .env_remove("SHELBI_WORKSPACE")
            .env("SHELBI_OPENCODE_SESSION", "ses_shelbi0123456789abcdef0123")
            .env_remove("SHELBI_OPENCODE_TITLE")
            .env_remove("TASK_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut first = String::new();
        BufReader::new(node.stdout.take().unwrap()).read_line(&mut first).unwrap();
        assert_eq!(first.trim(), "ready");

        let wait = Duration::from_secs(10);
        let no_paste = || -> Result<()> { panic!("an acked send must not paste") };
        let id = new_delivery_id();
        let sent = send_through_opencode_inbox_with(&Host::Local, &wt, "please rebase", &id, wait, no_paste).unwrap();
        assert_eq!(sent.status, SubmitStatus::Submitted { detail: "inbox_admitted" });
        assert_eq!(sent.degraded, None);

        // The same delivery id again: confirmed, not prompted twice.
        let again = send_through_opencode_inbox_with(&Host::Local, &wt, "please rebase", &id, wait, no_paste).unwrap();
        assert_eq!(again.status, SubmitStatus::Submitted { detail: "inbox_duplicate" });

        // OpenCode refuses the prompt: pasted instead, and the reason given.
        let mut pasted = 0;
        let refused = send_through_opencode_inbox_with(&Host::Local, &wt, "boom", &new_delivery_id(), wait, || {
            pasted += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(refused.status, SubmitStatus::DeliveredUnverified { detail: "inbox_fallback_paste" });
        assert_eq!(refused.degraded, Some("opencode-inbox-rejected"));
        assert_eq!(pasted, 1);

        let log = std::fs::read_to_string(&prompts).unwrap();
        let calls: Vec<serde_json::Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(calls.len(), 1, "{log}");
        assert_eq!(calls[0]["sessionID"], "ses_shelbi0123456789abcdef0123");
        assert_eq!(calls[0]["id"], format!("msg_{id}"));
        assert_eq!(calls[0]["text"], "please rebase");
        // Every entry was consumed.
        let left: Vec<_> = std::fs::read_dir(wt.join(OPENCODE_INBOX_REL)).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");

        // The plugin is gone: nothing reads the inbox, so the file is taken
        // back and the message pasted.
        drop(node.stdin.take());
        node.wait().unwrap();
        let mut pasted = 0;
        let silent = send_through_opencode_inbox_with(
            &Host::Local,
            &wt,
            "anyone there?",
            &new_delivery_id(),
            Duration::from_millis(500),
            || {
                pasted += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(silent.status, SubmitStatus::DeliveredUnverified { detail: "inbox_fallback_paste" });
        assert_eq!(silent.degraded, Some("opencode-inbox-silent"));
        assert_eq!(pasted, 1);
        let left: Vec<_> = std::fs::read_dir(wt.join(OPENCODE_INBOX_REL)).unwrap().collect();
        assert!(left.is_empty(), "the silent send's file is taken back: {left:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
