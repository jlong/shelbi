// Shelbi's OpenCode plugin. Shelbi writes it to `.opencode/plugins/shelbi.js`
// in every workspace worktree it launches OpenCode in and rewrites it on each
// launch, so edits here don't last. It is OpenCode's half of the hook contract
// Claude Code gets from `.claude/settings.local.json`: it runs the same
// runner-neutral scripts under `.shelbi/hooks/` on OpenCode's session events.
//
// - The pane title carries Shelbi's `shelbi:<state>` markers. OpenCode runs
//   this plugin in a server process with no controlling terminal, so the
//   launch exports the pane's tty as SHELBI_PANE_TTY and the marker scripts'
//   output is written there.
// - At the end of each turn, and while the session sits idle, `stop.sh`
//   drains new hub messages; a non-empty batch goes back into the session as
//   a synthetic message, which starts the next turn.
//
// A session Shelbi didn't launch (no SHELBI_PANE_TTY) is left alone.
import { execFile } from "node:child_process";
import { closeSync, existsSync, openSync, statSync, writeSync } from "node:fs";
import path from "node:path";

const HOOKS = ".shelbi/hooks";
const HOOK_TIMEOUT_MS = 15000;
const IDLE_DRAIN_MS = 5000;

export default {
  id: "shelbi",
  setup: async (api) => {
    const tty = process.env.SHELBI_PANE_TTY;
    const root = api.location?.directory || process.cwd();
    if (!tty || !existsSync(path.join(root, HOOKS))) return;

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
      // Answered either way, the turn carries on.
      if (/^(permission\.(replied|rejected)|form\.(replied|cancelled))$/.test(type)) {
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
    };
  },
};
