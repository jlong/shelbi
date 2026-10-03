//! Spike item 6: nesting inside tmux and GNU Screen.
//!
//! The production TUI is an ordinary full-screen program, so nesting works by
//! construction. What needs proving is *capability detection*: which terminal
//! features survive the outer multiplexer. This harness runs the `probe`
//! stand-in inside tmux and inside Screen, with the test itself emulating a
//! kitty-capable, truecolor outer terminal on a real PTY, and records:
//!
//!   * kitty keyboard: did the app's `CSI ? u` query get a reply back through
//!     the multiplexer (the harness answers it as a kitty terminal would)?
//!   * truecolor: what `TERM`/`COLORTERM` did the multiplexer hand the pane?
//!   * OSC 52: did the app's clipboard write reach the outer terminal?
//!
//! Observations are written to `target/nesting-findings.txt` for the writeup.
//! The test asserts only that the probe *ran* under each multiplexer (nesting
//! itself works); the capability values are findings, not pass/fail, because
//! they depend on the multiplexer's version and config — which is the point.
//!
//! Uses a private tmux socket (`-L`) and a dedicated Screen session so it never
//! touches the user's live multiplexer servers.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

const BIN: &str = env!("CARGO_BIN_EXE_rt-runtime");

fn have(cmd: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {cmd} >/dev/null 2>&1")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Default, Debug)]
struct Observed {
    saw_kitty_query: bool,
    saw_osc52: bool,
}

/// Run `argv` on a real PTY, emulating a kitty + truecolor outer terminal, and
/// return (probe findings file contents, harness observations). `extra_env` is
/// applied to the launched multiplexer.
fn run_nested(
    argv: &[String],
    extra_env: &[(&str, &str)],
    out_file: &PathBuf,
) -> Option<(String, Observed)> {
    let _ = fs::remove_file(out_file);

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .ok()?;

    let mut cmd = CommandBuilder::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.cwd(std::env::current_dir().ok()?);

    let mut child = pair.slave.spawn_command(cmd).ok()?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().ok()?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer().ok()?));
    let observed = Arc::new(Mutex::new(Observed::default()));

    // Reader/emulator thread: answer the kitty query, note OSC 52.
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let obs2 = Arc::clone(&observed);
    let writer2 = Arc::clone(&writer);
    let handle = std::thread::spawn(move || {
        let mut acc: Vec<u8> = Vec::new();
        let mut buf = [0u8; 2048];
        let mut replied = false;
        let mut da_replied = false;
        loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    acc.extend_from_slice(&buf[..n]);
                    // Primary device attributes (DA1): ESC [ c. tmux sends this
                    // at startup for feature detection; answer as a VT220-class
                    // terminal so tmux completes its probe rather than giving
                    // up and assuming a bare terminal.
                    if !da_replied && (contains(&acc, b"\x1b[c") || contains(&acc, b"\x1b[0c")) {
                        let _ = writer2.lock().unwrap().write_all(b"\x1b[?62;1;6c");
                        let _ = writer2.lock().unwrap().flush();
                        da_replied = true;
                    }
                    // Kitty flags query: ESC [ ? u
                    if contains(&acc, b"\x1b[?u") {
                        obs2.lock().unwrap().saw_kitty_query = true;
                        if !replied {
                            // Answer as a kitty terminal: ESC [ ? 1 u
                            let _ = writer2.lock().unwrap().write_all(b"\x1b[?1u");
                            let _ = writer2.lock().unwrap().flush();
                            replied = true;
                        }
                    }
                    // OSC 52 clipboard write: ESC ] 52 ;
                    if contains(&acc, b"\x1b]52;") {
                        obs2.lock().unwrap().saw_osc52 = true;
                    }
                    if acc.len() > 1 << 16 {
                        acc.drain(0..acc.len() - 4096);
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Wait for the probe to drop its findings file, then tear the child down.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !out_file.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    // Give the emulator a moment to see trailing OSC 52 / query bytes.
    std::thread::sleep(Duration::from_millis(300));

    let _ = child.kill();
    let _ = child.wait();
    let _ = stop_tx.send(());
    drop(writer);
    let _ = handle.join();

    let findings = fs::read_to_string(out_file).ok()?;
    let obs = std::mem::take(&mut *observed.lock().unwrap());
    Some((findings, obs))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn write_section(buf: &mut String, title: &str, findings: &str, obs: &Observed) {
    buf.push_str(&format!("== {title} ==\n"));
    buf.push_str(findings);
    buf.push_str(&format!(
        "harness_saw_kitty_query={}\nharness_saw_osc52_passthrough={}\n\n",
        obs.saw_kitty_query, obs.saw_osc52
    ));
}

#[test]
fn nesting_inside_tmux_and_screen() {
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let _ = fs::create_dir_all(&out_dir);
    let findings_path = out_dir.join("nesting-findings.txt");
    let mut report = String::new();
    let mut ran_any = false;

    // ---- Baseline: probe directly on a PTY, no multiplexer ----
    // This validates the harness itself: the emulated outer terminal must see
    // the kitty query and OSC 52 and answer the query, so that a `false` under
    // a multiplexer is a real passthrough gap, not a harness bug.
    {
        let probe_out = out_dir.join("probe-baseline.txt");
        let argv: Vec<String> = vec![BIN.into(), "probe".into(), "--out".into(), probe_out.display().to_string()];
        if let Some((findings, obs)) = run_nested(&argv, &[], &probe_out) {
            // The baseline alone does not count as "nesting tested"; it only
            // validates the harness.
            write_section(&mut report, "baseline (no multiplexer)", &findings, &obs);
            assert!(
                obs.saw_kitty_query && findings.contains("kitty_keyboard=yes"),
                "harness baseline broken: the emulated terminal did not complete the kitty \
                 handshake with the probe, so nesting results would be meaningless:\n{findings}"
            );
            assert!(
                obs.saw_osc52,
                "harness baseline broken: OSC 52 did not reach the emulated terminal:\n{findings}"
            );
        } else {
            panic!("baseline probe did not complete; cannot trust nesting results");
        }
    }

    // ---- tmux, extended-keys OFF (default) and ON (the one-line fix) ----
    if have("tmux") {
        let sock = format!("rt_spike_{}", std::process::id());
        for (label, extkeys) in [("tmux (extended-keys off)", "off"), ("tmux (extended-keys on)", "on")] {
            let probe_out = out_dir.join(format!("probe-tmux-{extkeys}.txt"));
            // One private server per run; -f /dev/null ignores user config.
            let shellcmd = format!(
                "{BIN} probe --out {}",
                probe_out.display()
            );
            let argv: Vec<String> = vec![
                "tmux".into(),
                "-f".into(),
                "/dev/null".into(),
                "-L".into(),
                sock.clone(),
                "set-option".into(),
                "-g".into(),
                "default-terminal".into(),
                "xterm-256color".into(),
                ";".into(),
                "set-option".into(),
                "-s".into(),
                "extended-keys".into(),
                extkeys.into(),
                ";".into(),
                "new-session".into(),
                "-x".into(),
                "80".into(),
                "-y".into(),
                "24".into(),
                shellcmd,
            ];
            // Resolve tmux via PATH (CommandBuilder needs an absolute-ish name;
            // it uses execvp semantics, so a bare name works).
            if let Some((findings, obs)) = run_nested(&argv, &[], &probe_out) {
                ran_any = true;
                write_section(&mut report, label, &findings, &obs);
            } else {
                report.push_str(&format!("== {label} ==\n(probe did not complete)\n\n"));
            }
            let _ = std::process::Command::new("tmux")
                .args(["-L", &sock, "kill-server"])
                .status();
        }
    } else {
        report.push_str("== tmux ==\n(tmux not on PATH; untested)\n\n");
    }

    // ---- GNU Screen ----
    if have("screen") {
        let probe_out = out_dir.join("probe-screen.txt");
        let session = format!("rtspike{}", std::process::id());
        // -c /dev/null: empty rc. Run the probe directly as the window program.
        let argv: Vec<String> = vec![
            "screen".into(),
            "-c".into(),
            "/dev/null".into(),
            "-S".into(),
            session.clone(),
            BIN.into(),
            "probe".into(),
            "--out".into(),
            probe_out.display().to_string(),
        ];
        if let Some((findings, obs)) = run_nested(&argv, &[], &probe_out) {
            ran_any = true;
            write_section(&mut report, "GNU Screen", &findings, &obs);
        } else {
            report.push_str("== GNU Screen ==\n(probe did not complete)\n\n");
        }
        let _ = std::process::Command::new("screen")
            .args(["-S", &session, "-X", "quit"])
            .status();
    } else {
        report.push_str("== GNU Screen ==\n(screen not on PATH; untested)\n\n");
    }

    fs::write(&findings_path, &report).unwrap();
    eprintln!("---- nesting findings ({}) ----\n{report}", findings_path.display());

    assert!(
        ran_any,
        "neither tmux nor Screen was available to test nesting:\n{report}"
    );
}
