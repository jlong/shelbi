// Shelbi's OpenCode plugin. Shelbi writes it to
// `.shelbi/opencode/plugins/shelbi/index.js` in every workspace worktree it
// launches OpenCode in, names that directory in the launch's config layer
// (`plugins` in OPENCODE_CONFIG_CONTENT), and rewrites it on each launch, so
// edits here don't last. It is OpenCode's half of the hook contract Claude
// Code gets from `.claude/settings.local.json`: it runs the same
// runner-neutral scripts under `.shelbi/hooks/` on OpenCode's session events.
//
// - The pane title carries Shelbi's `shelbi:<state>` markers. OpenCode runs
//   this plugin in a server process with no controlling terminal, so the
//   launch exports the pane's tty as SHELBI_PANE_TTY and the marker scripts'
//   output is written there.
// - At the end of each turn, and while the session sits idle, `stop.sh`
//   drains new hub messages; a non-empty batch goes back into the session as
//   a synthetic message, which starts the next turn.
// - Only one instance acts per worktree. OpenCode may run `setup` more than
//   once in one server (once per location), and two instances would both
//   drain `stop.sh` and deliver every message twice. The first to create
//   `.shelbi/opencode/plugin.lock` (holding its pid) acts; the rest stay
//   passive. A lock whose pid is gone is taken over.
// - The acting instance writes `.shelbi/opencode/plugin.alive` (plugin
//   version, pid). Shelbi waits for it after launch; without it the
//   workspace reports `state_observation=degraded` and Shelbi reads
//   OpenCode's dialogs off the screen instead.
//
// A session Shelbi didn't launch (no SHELBI_PANE_TTY) is left alone.
import { execFile } from "node:child_process";
import {
  closeSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  statSync,
  unlinkSync,
  writeFileSync,
  writeSync,
} from "node:fs";
import path from "node:path";

const HOOKS = ".shelbi/hooks";
const STATE = ".shelbi/opencode";
const LOCK = "plugin.lock";
const BEACON = "plugin.alive";
const PLUGIN_VERSION = 1;
const HOOK_TIMEOUT_MS = 15000;
const IDLE_DRAIN_MS = 5000;

// Instances in this process, by worktree: a second `setup` in the same
// server sees the first here even before reading the lock file.
const ACTIVE = Symbol.for("shelbi.opencode.active");
const active = (globalThis[ACTIVE] ??= new Set());

const pidAlive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    return err?.code === "EPERM";
  }
};

// Take the worktree's single-instance lock. Returns a release function, or
// null when another live instance holds it.
const claim = (root) => {
  if (active.has(root)) return null;
  const dir = path.join(root, STATE);
  const lock = path.join(dir, LOCK);
  try {
    mkdirSync(dir, { recursive: true });
  } catch {
    return null;
  }
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const fd = openSync(lock, "wx");
      try {
        writeSync(fd, `${process.pid}\n`);
      } finally {
        closeSync(fd);
      }
      active.add(root);
      let released = false;
      const release = () => {
        if (released) return;
        released = true;
        active.delete(root);
        try {
          if (parseInt(readFileSync(lock, "utf8"), 10) === process.pid) unlinkSync(lock);
        } catch {
          // Already gone.
        }
      };
      process.once("exit", release);
      return release;
    } catch (err) {
      if (err?.code !== "EEXIST") return null;
      let holder = 0;
      try {
        holder = parseInt(readFileSync(lock, "utf8"), 10);
      } catch {
        // Unreadable: treat it as stale.
      }
      // Another process that's still running holds it.
      if (holder && holder !== process.pid && pidAlive(holder)) return null;
      // A dead holder, or this pid with no instance registered above (a lock
      // left by an earlier process that had the same pid): take it over.
      try {
        unlinkSync(lock);
      } catch {
        return null;
      }
    }
  }
  return null;
};

// Tell Shelbi the plugin loaded and acts for this worktree.
const beacon = (root) => {
  const file = path.join(root, STATE, BEACON);
  const tmp = `${file}.${process.pid}.tmp`;
  try {
    writeFileSync(
      tmp,
      `${JSON.stringify({ plugin: "shelbi", version: PLUGIN_VERSION, pid: process.pid })}\n`,
    );
    renameSync(tmp, file);
  } catch {
    // Shelbi then reports the plugin as silent, which is the truth.
  }
};

export default {
  id: "shelbi",
  setup: async (api) => {
    const tty = process.env.SHELBI_PANE_TTY;
    const root = api.location?.directory || process.cwd();
    if (!tty || !existsSync(path.join(root, HOOKS))) return;
    const release = claim(root);
    if (!release) return;
    beacon(root);

    // Run one hook script from the worktree root (the scripts use relative
    // paths) and resolve with its stdout. Output still counts on a failed
    // exit: `stop.sh` advances its cursor last, so a batch it printed before
    // failing is delivered again next time, never lost.
    const run = (name, input = "") =>
      new Promise((resolve) => {
        const child = execFile(
          path.join(root, HOOKS, name),
          [],
          { cwd: root, env: process.env, timeout: HOOK_TIMEOUT_MS, maxBuffer: 1 << 20 },
          (_err, stdout) => resolve(String(stdout ?? "")),
        );
        child.stdin?.on("error", () => {});
        child.stdin?.end(input);
      });

    const writeTitle = (sequence) => {
      if (!sequence) return;
      try {
        const fd = openSync(tty, "w");
        try {
          writeSync(fd, sequence);
        } finally {
          closeSync(fd);
        }
      } catch {
        // A pane that closed mid-write has nobody left to read the title.
      }
    };

    // Sessions whose parent is another session are subagents. Their turns
    // don't end the pane's turn, but while one runs the pane is waiting on
    // background work rather than on a person.
    const parents = new Map();
    const parentOf = async (sessionID) => {
      if (parents.has(sessionID)) return parents.get(sessionID);
      let parent = null;
      try {
        parent = (await api.session.get({ sessionID }))?.parentID ?? null;
      } catch {
        // Unknown session: treat it as a main one.
      }
      parents.set(sessionID, parent);
      return parent;
    };
    const runningChildren = new Set();

    let marker = "";
    const mark = async (state, input) => {
      if (state === marker) return;
      marker = state;
      const script = state === "idle" ? "pane-idle.sh" : `pane-${state}.sh`;
      writeTitle(await run(script, input));
    };

    // Deliver whatever `stop.sh` drains. Returns whether a batch went in.
    let draining = false;
    const drain = async (sessionID) => {
      if (draining) return false;
      draining = true;
      try {
        const batch = (await run("stop.sh")).trim();
        if (!batch) return false;
        await api.session.synthetic({
          sessionID,
          description: "Shelbi messages",
          text: batch,
          metadata: { source: "shelbi" },
        });
        return true;
      } catch {
        return false;
      } finally {
        draining = false;
      }
    };

    // While the main session is idle, drain as soon as the hub appends to
    // this task's message log.
    const messageLog = process.env.TASK_ID
      ? path.join(root, ".shelbi", "messages", `${process.env.TASK_ID}.log`)
      : null;
    const logSize = () => {
      try {
        return statSync(messageLog).size;
      } catch {
        return 0;
      }
    };
    let idleSession = null;
    let drainedSize = messageLog ? logSize() : 0;
    const timer = messageLog
      ? setInterval(async () => {
          if (!idleSession) return;
          const size = logSize();
          if (size === drainedSize) return;
          drainedSize = size;
          const sessionID = idleSession;
          if (await drain(sessionID)) {
            idleSession = null;
            await mark("working");
          }
        }, IDLE_DRAIN_MS)
      : null;
    timer?.unref?.();

    await run("session-start.sh");

    const turnEnded = async (sessionID) => {
      if (await drain(sessionID)) return;
      drainedSize = messageLog ? logSize() : 0;
      idleSession = sessionID;
      const background = runningChildren.size > 0;
      // `pane-idle.sh` reads Claude's Stop payload; a non-empty
      // `background_tasks` list makes it write `shelbi:background`.
      const payload = background ? '{"background_tasks":[{"type":"subagent"}]}' : "{}";
      marker = "";
      await mark("idle", payload);
    };

    const handle = async (event) => {
      const type = event?.type ?? "";
      const data = event?.data ?? {};
      if (type === "session.created") {
        parents.set(data.sessionID, data.parentID ?? null);
        return;
      }
      if (type === "permission.asked" || type === "form.created") {
        await mark("blocked");
        return;
      }
      // Answered either way, the turn carries on. (A rejected ask is a
      // `permission.replied` with `reply: "reject"`.)
      if (type === "permission.replied" || type === "form.replied" || type === "form.cancelled") {
        if (marker === "blocked") await mark("working");
        return;
      }
      if (!type.startsWith("session.") || !data.sessionID) return;
      const child = (await parentOf(data.sessionID)) !== null;
      if (type === "session.execution.started") {
        if (child) {
          runningChildren.add(data.sessionID);
          return;
        }
        idleSession = null;
        await mark("working");
        return;
      }
      if (type.startsWith("session.execution.")) {
        if (child) {
          runningChildren.delete(data.sessionID);
          return;
        }
        await turnEnded(data.sessionID);
        return;
      }
      if (!child && (type === "session.inbox.enqueued" || type === "session.tool.called")) {
        idleSession = null;
        await mark("working");
      }
    };

    const controller = new AbortController();
    (async () => {
      for await (const event of api.event.subscribe({ signal: controller.signal })) {
        await handle(event).catch(() => {});
      }
    })().catch(() => {});
    return () => {
      controller.abort();
      if (timer) clearInterval(timer);
      release();
    };
  },
};
