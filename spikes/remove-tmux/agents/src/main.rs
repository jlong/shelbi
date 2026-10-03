//! rt-spike-agents — Phase 0 spike harness for running agents in a
//! Shelbi-owned PTY (no tmux), rendered by a ratatui widget.
//!
//! Subcommands:
//!   synth                 run the built-in synthetic agent (a deterministic
//!                         terminal-feature battery) as a PTY child
//!   probe  --cmd '<...>'  spawn a command in a PTY, run the query responder
//!                         with NO render client attached, capture what the
//!                         child asks and whether it boots; print a report
//!   bench  [--mb N]       spawn `synth --burst N` and measure parse throughput
//!                         and full-frame render cost
//!   attach --cmd '<...>'  spawn a command in a PTY and render it interactively
//!                         with the ratatui widget (live keys/mouse/paste)
//!
//! `synth`, `probe`, and `bench` are non-interactive and are what the spike's
//! recorded results come from. `attach` is the interactive render path; it
//! compiles and runs, but live human-driven verification is a follow-up.

// Some harness accessors are exercised only by the module unit tests (the
// deterministic encoder/emulator proofs); a throwaway spike need not gate each
// behind cfg(test).
#![allow(dead_code)]

mod input;
mod responder;
mod term;

use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use responder::{KittyFlags, Responder};
use term::Term;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str).unwrap_or("help");
    match sub {
        "synth" => run_synth(&args[2..]),
        "probe" => run_probe(&args[2..]),
        "bench" => run_bench(&args[2..]),
        "attach" => run_attach(&args[2..]),
        _ => {
            eprintln!(
                "rt-spike-agents <synth|probe|bench|attach> [opts]\n\
                 see module docs for options"
            );
            Ok(())
        }
    }
}

// ------- argument helpers (tiny, to avoid a clap dependency) -------

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

// ------- the synthetic agent child -------

/// Emits the full terminal-feature battery a full-screen agent exercises at
/// startup, so `probe`/`bench` can verify the harness answers and renders each
/// without needing a real agent (network/auth independent).
fn run_synth(args: &[String]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    if let Some(mb) = opt(args, "--burst") {
        let mb: usize = mb.parse().unwrap_or(4);
        return synth_burst(&mut out, mb);
    }

    // 1. Queries the responder must answer.
    out.write_all(b"\x1b[6n")?; // DSR cursor position
    out.write_all(b"\x1b[5n")?; // DSR status
    out.write_all(b"\x1b[c")?; // DA1
    out.write_all(b"\x1b[>c")?; // DA2
    out.write_all(b"\x1b[?u")?; // Kitty keyboard query
    out.write_all(b"\x1b]11;?\x07")?; // background color query
    out.write_all(b"\x1b]10;?\x07")?; // foreground color query

    // 2. Mode changes the emulator must observe.
    out.write_all(b"\x1b[?2004h")?; // bracketed paste
    out.write_all(b"\x1b[?1000h")?; // mouse click reporting
    out.write_all(b"\x1b[?1049h")?; // alternate screen
    out.write_all(b"\x1b[>1u")?; // push Kitty disambiguate flag

    // 3. Content the emulator must lay out correctly.
    out.write_all("ascii | 中文 | 😀🎉 | café\r\n".as_bytes())?;
    out.write_all(b"SYNTH-READY\r\n")?;
    out.flush()?;
    thread::sleep(Duration::from_millis(150));
    Ok(())
}

fn synth_burst(out: &mut impl Write, mb: usize) -> Result<()> {
    let total = mb * 1024 * 1024;
    // A line mixing color SGR, wide chars, and ascii — representative of heavy
    // agent output (diffs, logs, spinners).
    let line = "\x1b[32m+ added\x1b[0m \x1b[31m- removed\x1b[0m 中文 😀 normal text here 1234567890\r\n";
    let bytes = line.as_bytes();
    let mut written = 0;
    while written < total {
        out.write_all(bytes)?;
        written += bytes.len();
    }
    out.write_all(b"BURST-DONE\r\n")?;
    out.flush()?;
    Ok(())
}

// ------- PTY plumbing shared by probe/bench/attach -------

struct PtyChild {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    rx: mpsc::Receiver<Vec<u8>>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

fn spawn_in_pty(cmdline: &str, rows: u16, cols: u16) -> Result<PtyChild> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty")?;

    // Run the command line through `sh -c` so callers can pass a full command
    // with args/flags, mirroring how the orchestrator pane wrapper launches.
    let mut cmd = CommandBuilder::new("sh");
    cmd.arg("-c");
    cmd.arg(cmdline);
    cmd.cwd(std::env::current_dir()?);
    // A full-screen agent needs a sane TERM to emit the queries we test.
    cmd.env("TERM", "xterm-256color");
    // Strip every trace of tmux/screen so the child cannot fall back to a
    // multiplexer. This proves the agent boots in a bare Shelbi-owned PTY with
    // no `$TMUX_PANE` and no inherited multiplexer context — the target model.
    cmd.env_remove("TMUX");
    cmd.env_remove("TMUX_PANE");
    cmd.env_remove("STY");

    let child = pair.slave.spawn_command(cmd).context("spawn in pty")?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().context("clone reader")?;
    let writer = pair.master.take_writer().context("take writer")?;

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok(PtyChild {
        child,
        writer,
        rx,
        _master: pair.master,
    })
}

// ------- probe: boot an agent with no client attached -------

fn run_probe(args: &[String]) -> Result<()> {
    let cmdline = opt(args, "--cmd").context("probe needs --cmd '<command>'")?;
    let rows: u16 = opt(args, "--rows").and_then(|s| s.parse().ok()).unwrap_or(40);
    let cols: u16 = opt(args, "--cols").and_then(|s| s.parse().ok()).unwrap_or(120);
    let secs: u64 = opt(args, "--secs").and_then(|s| s.parse().ok()).unwrap_or(6);
    // --no-answer runs the identical read loop but never writes query replies,
    // the A/B that shows whether an agent depends on the responder (Codex exits;
    // Claude degrades) vs. not.
    let answer = !flag(args, "--no-answer");

    let mut pc = spawn_in_pty(cmdline, rows, cols)?;
    let mut term = Term::new(rows, cols);
    let kitty = KittyFlags::default();
    let mut responder = Responder::new(kitty.clone());

    let mut answered: Vec<String> = Vec::new();
    let mut total_bytes = 0usize;
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut exited_early = None;

    while Instant::now() < deadline {
        match pc.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                total_bytes += chunk.len();
                // Responder answers against the live emulator cursor.
                term.process(&chunk);
                for ans in responder.scan(&chunk, &term) {
                    answered.push(ans.query.to_string());
                    if answer {
                        // Write the reply back onto the child's stdin.
                        let _ = pc.writer.write_all(&ans.reply);
                        let _ = pc.writer.flush();
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(Some(status)) = pc.child.try_wait() {
                    exited_early = Some(status.exit_code());
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Is the child still alive at the deadline? (A full-screen agent that got
    // its query replies stays up; Codex exits if the cursor reply was late.)
    let alive = pc.child.try_wait().ok().flatten().is_none();
    let _ = pc.child.kill();

    println!("== probe report ==");
    println!("cmd:            {cmdline}");
    println!("responder:      {}", if answer { "ON" } else { "OFF (--no-answer)" });
    println!("bytes read:     {total_bytes}");
    println!("queries asked:  {}", answered.len());
    for q in dedup_counts(&answered) {
        println!("  - {q}");
    }
    println!("alternate screen entered: {}", term.alternate_screen());
    println!("bracketed paste enabled:  {}", term.bracketed_paste());
    println!("mouse reporting enabled:  {}", term.mouse_enabled());
    println!("kitty disambiguate flag:  {}", kitty.disambiguate_active());
    match exited_early {
        Some(code) => println!("CHILD EXITED EARLY (code {code}) — did it get its query replies?"),
        None if alive => println!("child still alive at +{secs}s (booted OK, no client attached)"),
        None => println!("child exited at/after deadline"),
    }
    Ok(())
}

fn dedup_counts(items: &[String]) -> Vec<String> {
    let mut seen: Vec<(String, usize)> = Vec::new();
    for it in items {
        if let Some(e) = seen.iter_mut().find(|(k, _)| k == it) {
            e.1 += 1;
        } else {
            seen.push((it.clone(), 1));
        }
    }
    seen.into_iter()
        .map(|(k, n)| if n > 1 { format!("{k} (x{n})") } else { k })
        .collect()
}

// ------- bench: redraw cost under heavy output -------

fn run_bench(args: &[String]) -> Result<()> {
    let mb: usize = opt(args, "--mb").and_then(|s| s.parse().ok()).unwrap_or(8);
    let rows: u16 = opt(args, "--rows").and_then(|s| s.parse().ok()).unwrap_or(40);
    let cols: u16 = opt(args, "--cols").and_then(|s| s.parse().ok()).unwrap_or(120);
    let self_exe = std::env::current_exe()?;
    let cmdline = format!("{} synth --burst {}", self_exe.display(), mb);

    let mut pc = spawn_in_pty(&cmdline, rows, cols)?;
    let mut term = Term::new(rows, cols);

    // Parse throughput: time feeding every byte into the emulator.
    let start = Instant::now();
    let mut total = 0usize;
    let mut chunks = 0usize;
    while let Ok(chunk) = pc.rx.recv_timeout(Duration::from_secs(10)) {
        total += chunk.len();
        chunks += 1;
        term.process(&chunk);
    }
    let parse_elapsed = start.elapsed();
    let _ = pc.child.wait();

    // Render cost: time a full-frame paint into an off-screen buffer, repeated.
    let area = ratatui::layout::Rect::new(0, 0, cols, rows);
    let frames = 2000;
    let render_start = Instant::now();
    for _ in 0..frames {
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(term::TermWidget(&term), area, &mut buf);
        std::hint::black_box(&buf);
    }
    let render_elapsed = render_start.elapsed();

    let mbps = (total as f64 / (1024.0 * 1024.0)) / parse_elapsed.as_secs_f64();
    let per_frame_us = render_elapsed.as_micros() as f64 / frames as f64;
    let max_fps = 1_000_000.0 / per_frame_us;

    println!("== bench report ==");
    println!("grid:              {rows}x{cols}");
    println!("bytes parsed:      {total} ({:.1} MiB)", total as f64 / 1048576.0);
    println!("chunks (PTY reads):{chunks}");
    println!("parse wall time:   {:?}", parse_elapsed);
    println!("parse throughput:  {mbps:.1} MiB/s");
    println!("render frames:     {frames}");
    println!("render per frame:  {per_frame_us:.1} us");
    println!("render ceiling:    {max_fps:.0} fps (single thread, uncapped)");
    Ok(())
}

// ------- attach: interactive ratatui render (compiles; smoke only) -------

fn run_attach(args: &[String]) -> Result<()> {
    use crossterm::event::{self, Event};
    let cmdline = opt(args, "--cmd").context("attach needs --cmd '<command>'")?;

    let mut terminal = ratatui::init();
    let size = terminal.size()?;
    let rows = size.height;
    let cols = size.width;

    let mut pc = spawn_in_pty(cmdline, rows, cols)?;
    let mut term = Term::new(rows, cols);
    let kitty = KittyFlags::default();
    let mut responder = Responder::new(kitty.clone());

    let result = (|| -> Result<()> {
        loop {
            // Drain child output.
            while let Ok(chunk) = pc.rx.try_recv() {
                term.process(&chunk);
                for ans in responder.scan(&chunk, &term) {
                    pc.writer.write_all(&ans.reply)?;
                }
                pc.writer.flush()?;
            }
            if pc.child.try_wait()?.is_some() {
                break;
            }
            terminal.draw(|f| {
                f.render_widget(term::TermWidget(&term), f.area());
            })?;
            // Forward UI input to the agent.
            if event::poll(Duration::from_millis(16))? {
                match event::read()? {
                    Event::Key(k) => {
                        // Ctrl+] detaches (does not kill the agent).
                        if k.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                            && matches!(k.code, crossterm::event::KeyCode::Char(']'))
                        {
                            break;
                        }
                        let bytes = input::encode_key(&k, kitty.disambiguate_active());
                        pc.writer.write_all(&bytes)?;
                        pc.writer.flush()?;
                    }
                    Event::Mouse(m) => {
                        if term.mouse_enabled() {
                            let bytes = input::encode_mouse_sgr(&m, m.column + 1, m.row + 1);
                            pc.writer.write_all(&bytes)?;
                            pc.writer.flush()?;
                        }
                    }
                    Event::Paste(text) => {
                        let bytes = input::encode_paste(&text, term.bracketed_paste());
                        pc.writer.write_all(&bytes)?;
                        pc.writer.flush()?;
                    }
                    Event::Resize(c, r) => {
                        term.resize(r, c);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    })();

    ratatui::restore();
    let _ = pc.child.kill();
    if let Err(e) = result {
        bail!("attach loop: {e}");
    }
    Ok(())
}
