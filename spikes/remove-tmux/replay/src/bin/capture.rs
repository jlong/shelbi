//! Record a real program's PTY output into a fixture the replay tests read.
//!
//! Usage:
//!   capture <outfile> nvim-file     # nvim on a code file (full-screen, color)
//!   capture <outfile> shell-nvim    # a shell with markers, then nvim opened
//!   capture <outfile> shell-less    # a shell with markers, then less opened
//!
//! Each scenario drives input with fixed delays, captures all output at an
//! 80x24 PTY, and stops while the full-screen program is still open (on the
//! alternate screen) so the fixture holds the mid-session state replay must
//! reconstruct. Re-run to refresh a fixture; the recorded bytes are what the
//! tests assert on, so capture is not in the test path.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: capture <outfile> <nvim-file|shell-nvim|shell-less>");
        std::process::exit(2);
    }
    let outfile = &args[1];
    let scenario = &args[2];

    let bytes = match scenario.as_str() {
        "nvim-file" => capture_nvim_file(),
        "shell-nvim" => capture_shell_then(&["nvim", "-u", "NONE", "-N"]),
        "shell-less" => capture_shell_then(&["less"]),
        other => {
            eprintln!("unknown scenario: {other}");
            std::process::exit(2);
        }
    };

    std::fs::write(outfile, &bytes).expect("write fixture");
    eprintln!("wrote {} bytes to {outfile}", bytes.len());
}

fn pty_size() -> PtySize {
    PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Spawn nvim directly on a small highlighted file.
fn capture_nvim_file() -> Vec<u8> {
    let code = "fn main() {\n    let xs = vec![1, 2, 3];\n    for x in &xs {\n        println!(\"{x}\");\n    }\n}\n";
    let path = std::env::temp_dir().join("rt-spike-capture.rs");
    std::fs::write(&path, code).unwrap();

    let mut cmd = CommandBuilder::new("nvim");
    cmd.args(["-u", "NONE", "-N", "-c", "syntax on", path.to_str().unwrap()]);
    run(cmd, |_w| {
        thread::sleep(Duration::from_millis(1500));
    })
}

/// Spawn an interactive shell, echo markers onto the normal screen, then open
/// a full-screen program, and stop while it is open.
fn capture_shell_then(program: &[&str]) -> Vec<u8> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.arg("-i");
    let prog: Vec<String> = program.iter().map(|s| s.to_string()).collect();
    run(cmd, move |w| {
        thread::sleep(Duration::from_millis(400));
        w.write_all(b"echo SHELL_UNDERNEATH_MARKER\n").unwrap();
        w.write_all(b"echo second-shell-line\n").unwrap();
        thread::sleep(Duration::from_millis(400));
        w.write_all(format!("{}\n", prog.join(" ")).as_bytes())
            .unwrap();
        thread::sleep(Duration::from_millis(1500));
    })
}

/// Spawn `cmd` at an 80x24 PTY, run `drive` against the master writer, capture
/// all output, then kill the child and return the bytes.
fn run<F>(mut cmd: CommandBuilder, drive: F) -> Vec<u8>
where
    F: FnOnce(&mut Box<dyn Write + Send>),
{
    cmd.env("TERM", "xterm-256color");
    cmd.env("LANG", "en_US.UTF-8");

    let pty = native_pty_system();
    let pair = pty.openpty(pty_size()).expect("openpty");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("reader");
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf2 = Arc::clone(&buf);
    let reader_thread = thread::spawn(move || {
        let mut tmp = [0u8; 8192];
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => buf2.lock().unwrap().extend_from_slice(&tmp[..n]),
                Err(_) => break,
            }
        }
    });

    {
        let mut writer = pair.master.take_writer().expect("writer");
        drive(&mut writer);
    }

    let _ = child.kill();
    let _ = child.wait();
    drop(pair.master);
    let _ = reader_thread.join();

    let out = buf.lock().unwrap().clone();
    out
}
