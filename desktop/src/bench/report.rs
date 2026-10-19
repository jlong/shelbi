//! The bench's numbers: computed from the raw samples, printed as lines and
//! a Markdown table (for the status doc), and appended as JSON.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use serde::Serialize;
use shelbi_term::view::TerminalView;

/// Percentiles of a set of durations, in milliseconds.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Spread {
    pub n: usize,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

impl Spread {
    pub fn of(samples: &[Duration]) -> Spread {
        if samples.is_empty() {
            return Spread::default();
        }
        let mut v: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        let at = |q: f64| v[((v.len() as f64 * q) as usize).min(v.len() - 1)];
        Spread {
            n: v.len(),
            p50: at(0.50),
            p95: at(0.95),
            p99: at(0.99),
            max: *v.last().unwrap(),
        }
    }

    fn short(&self) -> String {
        format!(
            "p50 {:.2} / p95 {:.2} / p99 {:.2} / max {:.1} ms (n={})",
            self.p50, self.p95, self.p99, self.max, self.n
        )
    }
}

/// One stream case.
#[derive(Debug, Clone, Serialize)]
pub struct CaseResult {
    pub input: String,
    /// `session` (through a real PTY and session) or `direct` (straight into
    /// the pane).
    pub source: String,
    pub input_bytes: usize,
    /// Bytes the pane fed its emulator (less than the input when the session
    /// dropped a lagging client to a resync).
    pub fed_bytes: u64,
    /// From the first output to the last.
    pub stream_s: f64,
    pub mib_per_s: f64,
    pub frames: usize,
    pub fps: f64,
    /// Root render through paint, per frame.
    pub frame_ms: Spread,
    /// Display intervals missed while output was arriving.
    pub dropped: usize,
    /// Share of one core, start to drained.
    pub cpu_pct: f64,
    /// Start of a palette open (on another thread) to the frame painting it.
    pub palette_ms: Spread,
    pub resyncs: u32,
    /// Main-thread time spent feeding the emulator, in all.
    pub feed_ms: f64,
    /// The pane's grid against a second client that saw the same stream.
    pub grid: String,
    /// The pane's grid against the session's own replay.
    pub grid_replay: String,
    pub timed_out: bool,
}

impl CaseResult {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input: &str,
        source: &str,
        input_bytes: usize,
        fed_bytes: u64,
        stream: Duration,
        wall: Duration,
        cpu: Duration,
        frames: &[(Instant, Instant)],
        refresh: Duration,
        palette: &[Duration],
        resyncs: u32,
    ) -> CaseResult {
        let times: Vec<Duration> = frames.iter().map(|(s, e)| *e - *s).collect();
        let dropped = frames
            .windows(2)
            .map(|w| {
                let gap = w[1].1 - w[0].1;
                (gap.as_secs_f64() / refresh.as_secs_f64()).round().max(1.0) as usize - 1
            })
            .sum();
        let secs = stream.as_secs_f64().max(1e-6);
        CaseResult {
            input: input.to_string(),
            source: source.to_string(),
            input_bytes,
            fed_bytes,
            stream_s: stream.as_secs_f64(),
            mib_per_s: fed_bytes as f64 / 1024.0 / 1024.0 / secs,
            frames: frames.len(),
            fps: frames.len() as f64 / secs,
            frame_ms: Spread::of(&times),
            dropped,
            cpu_pct: 100.0 * cpu.as_secs_f64() / wall.as_secs_f64().max(1e-6),
            palette_ms: Spread::of(palette),
            resyncs,
            feed_ms: 0.0,
            grid: String::new(),
            grid_replay: String::new(),
            timed_out: false,
        }
    }

    pub fn print(&self) {
        println!("\n== {} via {} ({} bytes)", self.input, self.source, self.input_bytes);
        println!(
            "   streamed {} bytes in {:.2} s ({:.1} MiB/s), {} resync(s){}",
            self.fed_bytes,
            self.stream_s,
            self.mib_per_s,
            self.resyncs,
            if self.timed_out { ", TIMED OUT" } else { "" }
        );
        println!(
            "   {} frames, {:.1} fps, {} dropped; CPU {:.0}% of a core",
            self.frames, self.fps, self.dropped, self.cpu_pct
        );
        println!("   frame time  {}", self.frame_ms.short());
        println!("   palette     {}", self.palette_ms.short());
        println!("   feeding     {:.0} ms on the UI thread", self.feed_ms);
        println!("   grid        {} (vs. a second client)", self.grid);
        println!("               {} (vs. the session's replay)", self.grid_replay);
    }
}

/// The idle case.
#[derive(Debug, Clone, Serialize)]
pub struct IdleResult {
    pub attached: bool,
    pub wall_s: f64,
    pub frames: usize,
    pub cpu_pct: f64,
}

impl IdleResult {
    pub fn print(&self) {
        println!(
            "\n== idle (attached: {}): {} frames in {:.1} s, CPU {:.2}% of a core",
            self.attached, self.frames, self.wall_s, self.cpu_pct
        );
    }
}

/// The many-terminals case.
#[derive(Debug, Clone, Serialize)]
pub struct ManyResult {
    pub sessions: usize,
    pub rate_kib_s: u64,
    /// The quiet view shown while the others stream.
    pub hidden_frames: usize,
    pub hidden_cpu_pct: f64,
    pub hidden_wall_s: f64,
    /// Show a streaming view until a frame paints its content.
    pub switch_ms: Spread,
    pub switch_failures: Vec<String>,
    /// Of which: the connect (discover, hello, info, attach) off the UI
    /// thread, every connect the view made.
    pub connect_ms: Spread,
    /// While a streaming view is shown.
    pub shown_frame_ms: Spread,
    pub shown_cpu_pct: f64,
}

impl ManyResult {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        connects: &[Duration],
        sessions: usize,
        rate: u64,
        hidden_wall: Duration,
        hidden_frames: usize,
        hidden_cpu: Duration,
        switches: Vec<(String, Option<Duration>)>,
        shown_frames: &[Duration],
        shown_cpu: &[f64],
    ) -> ManyResult {
        let ok: Vec<Duration> = switches.iter().filter_map(|(_, d)| *d).collect();
        ManyResult {
            sessions,
            rate_kib_s: rate / 1024,
            hidden_frames,
            hidden_cpu_pct: 100.0 * hidden_cpu.as_secs_f64() / hidden_wall.as_secs_f64(),
            hidden_wall_s: hidden_wall.as_secs_f64(),
            switch_ms: Spread::of(&ok),
            switch_failures: switches
                .iter()
                .filter(|(_, d)| d.is_none())
                .map(|(n, _)| n.clone())
                .collect(),
            connect_ms: Spread::of(connects),
            shown_frame_ms: Spread::of(shown_frames),
            shown_cpu_pct: shown_cpu.iter().sum::<f64>() / shown_cpu.len().max(1) as f64,
        }
    }

    pub fn print(&self) {
        println!(
            "\n== many: {} sessions streaming {} KiB/s each",
            self.sessions, self.rate_kib_s
        );
        println!(
            "   hidden (a quiet view shown): {} frames in {:.1} s, CPU {:.1}%",
            self.hidden_frames, self.hidden_wall_s, self.hidden_cpu_pct
        );
        println!("   switch to a streaming view: {}", self.switch_ms.short());
        println!("   of which the connect: {}", self.connect_ms.short());
        if !self.switch_failures.is_empty() {
            println!("   switches that never painted: {:?}", self.switch_failures);
        }
        println!(
            "   while one streams: frame {}, CPU {:.1}%",
            self.shown_frame_ms.short(),
            self.shown_cpu_pct
        );
    }
}

/// Everything one run measured.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub label: String,
    pub when: String,
    pub load_average: Option<f64>,
    pub cases: Vec<CaseResult>,
    pub idle: Option<IdleResult>,
    pub many: Option<ManyResult>,
}

impl Report {
    pub fn new(label: String) -> Report {
        Report {
            label,
            when: chrono::Local::now().to_rfc3339(),
            load_average: load_average(),
            cases: Vec::new(),
            idle: None,
            many: None,
        }
    }

    /// The Markdown table the status doc carries.
    pub fn print_table(&self) {
        println!(
            "\n{} (load average {:.0} at start)\n",
            if self.label.is_empty() { "results" } else { &self.label },
            self.load_average.unwrap_or(0.0)
        );
        println!("| Input | Source | MiB/s | fps | frame p50 / p95 / max (ms) | dropped | CPU | palette p50 / max (ms) | resyncs | grid |");
        println!("|---|---|---|---|---|---|---|---|---|---|");
        for c in &self.cases {
            println!(
                "| {} | {} | {:.1} | {:.0} | {:.2} / {:.2} / {:.1} | {} | {:.0}% | {:.1} / {:.1} | {} | {} |",
                c.input,
                c.source,
                c.mib_per_s,
                c.fps,
                c.frame_ms.p50,
                c.frame_ms.p95,
                c.frame_ms.max,
                c.dropped,
                c.cpu_pct,
                c.palette_ms.p50,
                c.palette_ms.max,
                c.resyncs,
                c.grid
            );
        }
        if let Some(i) = &self.idle {
            println!("\nIdle: {} frames in {:.0} s, CPU {:.2}%", i.frames, i.wall_s, i.cpu_pct);
        }
        if let Some(m) = &self.many {
            println!(
                "Many ({}x {} KiB/s): hidden {} frames / CPU {:.1}%; switch p50 {:.0} ms, max {:.0} ms; shown frame p95 {:.2} ms, CPU {:.1}%",
                m.sessions,
                m.rate_kib_s,
                m.hidden_frames,
                m.hidden_cpu_pct,
                m.switch_ms.p50,
                m.switch_ms.max,
                m.shown_frame_ms.p95,
                m.shown_cpu_pct
            );
        }
    }

    pub fn append_json(&self, path: &Path) {
        let line = serde_json::to_string(self).expect("serialize the report");
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open the JSON file");
        writeln!(f, "{line}").expect("write the JSON");
        println!("appended to {}", path.display());
    }
}

fn load_average() -> Option<f64> {
    let mut avg = [0f64; 3];
    // SAFETY: getloadavg writes at most 3 doubles into our array.
    (unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) } >= 1).then_some(avg[0])
}

/// Compare two views' visible grids cell by cell (character, combining
/// marks, colors, attribute flags) and their cursors: `identical`, or how
/// many cells differ and the first one.
pub fn grid_diff(got: &TerminalView, want: &TerminalView) -> String {
    let (g, w) = (got.emulator(), want.emulator());
    if g.size() != w.size() {
        return format!("size differs: {:?} vs {:?}", g.size(), w.size());
    }
    let (gg, wg) = (g.grid(), w.grid());
    let mut differ = 0;
    let mut first = None;
    let mut first_at = None;
    for row in 0..gg.screen_lines() {
        for col in 0..gg.columns() {
            let a = &gg[Line(row as i32)][Column(col)];
            let b = &wg[Line(row as i32)][Column(col)];
            let same = a.c == b.c
                && a.fg == b.fg
                && a.bg == b.bg
                && a.flags == b.flags
                && a.zerowidth() == b.zerowidth();
            if !same {
                differ += 1;
                first.get_or_insert_with(|| {
                    format!(
                        "{:?} fg {:?} bg {:?} {:?} vs {:?} fg {:?} bg {:?} {:?}",
                        a.c, a.fg, a.bg, a.flags, b.c, b.fg, b.bg, b.flags
                    )
                });
                first_at.get_or_insert((row, col));
            }
        }
    }
    let cursor = (g.cursor().map(|c| (c.row, c.col)), w.cursor().map(|c| (c.row, c.col)));
    match (differ, first, first_at) {
        (0, _, _) if cursor.0 == cursor.1 => "identical".into(),
        (0, _, _) => format!("cells identical, cursor {:?} vs {:?}", cursor.0, cursor.1),
        (n, Some(what), Some((r, c))) => format!("{n} cells differ (first at {r},{c}: {what})"),
        (n, _, _) => format!("{n} cells differ"),
    }
}
