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
// - Shelbi launches the pane on a session it names (`--session`, exported as
//   SHELBI_OPENCODE_SESSION). The plugin acts on that session and its
//   subagents only, so another session opened in the same TUI is the user's
//   own. It titles the session SHELBI_OPENCODE_TITLE
//   (`Shelbi · <workspace> · <task>`) and puts the title back if it changes.
// - `shelbi send` reaches the session through `.shelbi/opencode/inbox/`: one
//   `<delivery-id>.json` per message. The plugin claims each by renaming it
//   to `<delivery-id>.taken` (Shelbi takes back an unclaimed file when no
//   answer comes, so only one side ever has it), admits the text with
//   `session.prompt({id: "msg_<delivery-id>"})`, and answers in
//   `.shelbi/opencode/ack/<delivery-id>.json` with `admitted`, `duplicate`
//   or `failed`. Nothing is typed into the TUI, so the user's draft in the
//   composer stays put. An id is admitted once: the plugin remembers the ids
//   it admitted, and OpenCode answers a repeated prompt id with the original
//   admission.
//
// - In the orchestrator's pane (SHELBI_OPENCODE_ROLE=orchestrator) the
//   plugin is the awareness plugin instead: the same contract Claude's
//   `shelbi-awareness` plugin runs, through the same commands. It runs
//   `shelbi orchestrator events stream --json` for the whole session and
//   admits each batch with `session.prompt`: queued, so an idle orchestrator
//   starts a turn on it at once, or steered into a running turn when it's
//   urgent. A batch that is only the board digest is added with
//   `session.synthetic({resume: false})`, which starts no turn. When a turn
//   starts it calls `shelbi orchestrator hook user-prompt-submit` (anything
//   the stream hasn't delivered yet goes in as context), and when one ends
//   `hook stop` (an urgent event still queued sends the orchestrator back to
//   work, once). `hook session-start` runs as the plugin loads; its answer
//   goes in with the first turn. The Shelbi CLI owns the queue, its cursor,
//   its lock and its acks; the plugin only carries batches across.
//
// A pane Shelbi didn't launch (no SHELBI_PANE_TTY or SHELBI_OPENCODE_SESSION)
// is left alone.
import { execFile, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import {
  closeSync,
  existsSync,
  mkdirSync,
  openSync,
  readdirSync,
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
const PLUGIN_VERSION = 3;
const HOOK_TIMEOUT_MS = 15000;
const IDLE_DRAIN_MS = 5000;
const INBOX = "inbox";
const ACK = "ack";
const INBOX_POLL_MS = 250;
// Shelbi's delivery ids have the shape of OpenCode's own message ids, so a
// sent message sorts by its send time among the session's messages.
const DELIVERY_ID = /^[0-9a-f]{12}[0-9A-Za-z]{14}$/;
// How far up a subagent's parents the plugin looks for its own session.
const MAX_SESSION_DEPTH = 8;
// The orchestrator's role (SHELBI_OPENCODE_ROLE).
const ORCHESTRATOR = "orchestrator";
// The event stream is restarted after it exits, backing off to this.
const STREAM_BACKOFF_MAX_MS = 60000;
// Attempts at admitting one batch before it's given up.
const ADMIT_ATTEMPTS = 3;
const EVENTS_HEADER =
  "Shelbi board events (already acknowledged; react to each through your reaction rules):";
const DIGEST_HEADER =
  "Shelbi board digest (already acknowledged; apply it through your digest reaction rule):";

// A fresh id in the shape of OpenCode's ascending message ids (and of the
// ids Shelbi mints for its inbox): 12 hex characters of the low 48 bits of
// millis * 4096 + a counter, then 14 base62 characters.
const BASE62 = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
let idCounter = 0;
const newMessageId = () => {
  const stamp =
    (BigInt(Date.now()) * 4096n + BigInt(idCounter++ % 4096)) & 0xffffffffffffn;
  let out = stamp.toString(16).padStart(12, "0");
  for (const byte of randomBytes(14)) out += BASE62[byte % 62];
  return `msg_${out}`;
};

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

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

// The orchestrator's half: board events in through the awareness contract.
// See the header.
const orchestrate = ({ api, own, root, title, applyTitle, isTitled, release }) => {
  const shelbi = process.env.SHELBI_BIN || "shelbi";
  const env = process.env;

  // Run `shelbi orchestrator hook <event>` with `input` as its JSON stdin and
  // resolve with its JSON answer, or null for none. A hook never fails the
  // turn: anything unexpected is no answer.
  const hook = (event, input) =>
    new Promise((resolve) => {
      const child = execFile(
        shelbi,
        ["orchestrator", "hook", event],
        { cwd: root, env, timeout: HOOK_TIMEOUT_MS, maxBuffer: 1 << 20 },
        (_err, stdout) => {
          const text = String(stdout ?? "").trim();
          if (!text) return resolve(null);
          try {
            resolve(JSON.parse(text));
          } catch {
            resolve(null);
          }
        },
      );
      child.stdin?.on("error", () => {});
      child.stdin?.end(JSON.stringify(input ?? {}));
    });
  const contextOf = (answer) => {
    const context = answer?.hookSpecificOutput?.additionalContext;
    return typeof context === "string" && context.trim() ? context : null;
  };

  // Whether the orchestrator's own session is running a turn.
  let busy = false;
  // The turn running now answers our own stop-hook block, so its end mustn't
  // block again (Claude's `stop_hook_active`).
  let continuing = false;

  // Admit a prompt, retrying with the same id: OpenCode answers a repeated
  // id with the original admission, so a retry never admits it twice.
  const prompt = async (text, delivery, kind) => {
    const id = newMessageId();
    for (let attempt = 1; ; attempt++) {
      try {
        await api.session.prompt({
          sessionID: own,
          id,
          text,
          delivery,
          metadata: { source: "shelbi", kind },
        });
        return true;
      } catch {
        if (attempt >= ADMIT_ATTEMPTS) return false;
        // A steer can lose the race with the turn ending; queue it instead.
        delivery = "queue";
        await sleep(250 * attempt);
      }
    }
  };
  // Add context without starting a turn. In a running turn it is taken in at
  // the next step; otherwise with the next turn.
  const note = async (text, description) => {
    try {
      await api.session.synthetic({
        sessionID: own,
        text,
        description,
        resume: false,
        metadata: { source: "shelbi" },
      });
      return true;
    } catch {
      return false;
    }
  };

  // One batch from the stream: `{lines, urgent, digest}`.
  const admitBatch = async (batch) => {
    const lines = Array.isArray(batch?.lines)
      ? batch.lines.filter((line) => typeof line === "string" && line.trim())
      : [];
    if (!lines.length) return;
    if (batch.digest) {
      await note(`${DIGEST_HEADER}\n${lines.join("\n")}`, "Shelbi board digest");
      return;
    }
    const delivery = batch.urgent && busy ? "steer" : "queue";
    await prompt(`${EVENTS_HEADER}\n${lines.join("\n")}`, delivery, "events");
  };

  // Batches are admitted one at a time, in the order the stream printed them.
  let admitting = Promise.resolve();
  const enqueue = (line) => {
    let batch;
    try {
      batch = JSON.parse(line);
    } catch {
      // Not a batch: a plain line (a notice) stands for itself.
      batch = { lines: [line], urgent: false, digest: false };
    }
    admitting = admitting.then(() => admitBatch(batch)).catch(() => {});
  };

  let stopped = false;
  let stream = null;
  let streaming = false;
  let backoff = 1000;
  // The stream starts once the session exists (the TUI creates it on a first
  // start): a batch admitted into a session that isn't there yet would fail,
  // and the stream has already acked it.
  const ensureStream = () => {
    if (streaming) return;
    streaming = true;
    startStream();
  };
  const startStream = () => {
    if (stopped) return;
    const started = Date.now();
    let child;
    try {
      child = spawn(shelbi, ["orchestrator", "events", "stream", "--json"], {
        cwd: root,
        env,
        stdio: ["ignore", "pipe", "ignore"],
      });
    } catch {
      child = null;
    }
    const restart = () => {
      if (stream === child) stream = null;
      if (stopped) return;
      // A stream that ran a good while was healthy: start over at the bottom.
      if (Date.now() - started > STREAM_BACKOFF_MAX_MS) backoff = 1000;
      const timer = setTimeout(startStream, backoff);
      timer.unref?.();
      backoff = Math.min(backoff * 2, STREAM_BACKOFF_MAX_MS);
    };
    if (!child) return restart();
    stream = child;
    let buffer = "";
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      buffer += chunk;
      let end;
      while ((end = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, end).trim();
        buffer = buffer.slice(end + 1);
        if (line) enqueue(line);
      }
    });
    child.on("error", () => {});
    child.on("close", restart);
  };
  const stopStream = () => {
    stopped = true;
    try {
      stream?.kill("SIGTERM");
    } catch {
      // Already gone.
    }
  };
  process.once("exit", stopStream);

  // The session-start answer, which goes in with the session's first turn.
  // A session that already exists is a resume (a reload, a restart), and the
  // hook then restates how events arrive.
  const startup = (async () => {
    let resumed = false;
    try {
      resumed = Boolean(await api.session.get({ sessionID: own }));
    } catch {
      // Not created yet: a first start.
    }
    if (resumed) ensureStream();
    return contextOf(await hook("session-start", { source: resumed ? "resume" : "startup" }));
  })().catch(() => null);
  let startTaken = false;

  const turnStarted = async () => {
    busy = true;
    const context = [];
    if (!startTaken) {
      startTaken = true;
      const start = await startup;
      if (start) context.push(start);
    }
    const pending = contextOf(await hook("user-prompt-submit", {}));
    if (pending) context.push(pending);
    if (context.length) await note(context.join("\n\n"), "Shelbi board events");
  };
  const turnSucceeded = async () => {
    busy = false;
    const answered = continuing;
    continuing = false;
    // Already continuing because of a stop block: never block again, so the
    // orchestrator can't be held in a loop.
    if (answered) return;
    const answer = await hook("stop", { stop_hook_active: false });
    if (answer?.decision !== "block" || typeof answer.reason !== "string") return;
    continuing = true;
    if (!(await prompt(answer.reason, "queue", "stop"))) continuing = false;
  };

  const handle = async (event) => {
    const type = event?.type ?? "";
    const data = event?.data ?? {};
    if (type === "session.created") {
      if (data.sessionID === own) {
        ensureStream();
        await applyTitle();
      }
      return;
    }
    if (type === "session.renamed") {
      if (data.sessionID === own && title && data.title !== title) await applyTitle();
      return;
    }
    // Subagents' turns are part of the orchestrator's, not turns of its own.
    if (data.sessionID !== own) return;
    if (type === "session.execution.started") {
      ensureStream();
      if (!isTitled()) await applyTitle();
      await turnStarted();
      return;
    }
    if (type === "session.execution.succeeded") {
      await turnSucceeded();
      return;
    }
    if (type === "session.execution.failed" || type === "session.execution.interrupted") {
      busy = false;
      continuing = false;
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
    stopStream();
    process.removeListener("exit", stopStream);
    release();
  };
};

export default {
  id: "shelbi",
  setup: async (api) => {
    const tty = process.env.SHELBI_PANE_TTY;
    const own = process.env.SHELBI_OPENCODE_SESSION;
    const title = process.env.SHELBI_OPENCODE_TITLE;
    const orchestrator = process.env.SHELBI_OPENCODE_ROLE === ORCHESTRATOR;
    const root = api.location?.directory || process.cwd();
    if (!tty || !own) return;
    // A worker runs the worktree's hook scripts; without them it has nothing
    // to do. The orchestrator's workdir has none.
    if (!orchestrator && !existsSync(path.join(root, HOOKS))) return;
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
    const parents = new Map([[own, null]]);
    const parentOf = async (sessionID) => {
      if (parents.has(sessionID)) return parents.get(sessionID);
      let parent = null;
      try {
        parent = (await api.session.get({ sessionID }))?.parentID ?? null;
      } catch {
        // Unknown session: treat it as a top-level one, so not ours.
      }
      parents.set(sessionID, parent);
      return parent;
    };
    // Whether `sessionID` is the pane's session or one of its subagents.
    const ours = async (sessionID) => {
      let id = sessionID;
      for (let depth = 0; id && depth < MAX_SESSION_DEPTH; depth++) {
        if (id === own) return true;
        id = await parentOf(id);
      }
      return false;
    };
    const runningChildren = new Set();

    // Title the pane's session so the user can tell it from their own. The
    // session may not exist yet when the plugin loads (the TUI creates it),
    // so this is tried again when it appears, and the title is put back
    // whenever something else renames it (OpenCode's own title agent).
    let titled = false;
    const applyTitle = async () => {
      if (!title) return;
      try {
        await api.session.update({ sessionID: own, title });
        titled = true;
      } catch {
        // Not created yet: `session.created` or the first turn retries.
      }
    };

    if (orchestrator) {
      return orchestrate({ api, own, root, title, applyTitle, isTitled: () => titled, release });
    }

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

    // Verified sends: admit each inbox entry into the pane's session and
    // answer it. Entries are taken in id (send) order, one at a time.
    const inboxDir = path.join(root, STATE, INBOX);
    const ackDir = path.join(root, STATE, ACK);
    const admitted = new Set();
    const answer = (id, status, error) => {
      try {
        mkdirSync(ackDir, { recursive: true });
        const file = path.join(ackDir, `${id}.json`);
        const tmp = path.join(ackDir, `.${id}.${process.pid}.tmp`);
        writeFileSync(tmp, `${JSON.stringify(error ? { id, status, error } : { id, status })}\n`);
        renameSync(tmp, file);
      } catch {
        // Shelbi then takes the message for lost and says so.
      }
    };
    const admit = async (id) => {
      const entry = path.join(inboxDir, `${id}.json`);
      const taken = path.join(inboxDir, `${id}.taken`);
      try {
        renameSync(entry, taken);
      } catch {
        // Shelbi took it back first.
        return;
      }
      try {
        if (admitted.has(id)) {
          answer(id, "duplicate");
          return;
        }
        let text;
        try {
          const body = JSON.parse(readFileSync(taken, "utf8"));
          if (body?.id !== id) throw new Error("its id doesn't match its name");
          text = typeof body.text === "string" ? body.text : "";
        } catch (err) {
          answer(id, "failed", `unreadable entry: ${err?.message ?? err}`);
          return;
        }
        if (!text.trim()) {
          answer(id, "failed", "empty message");
          return;
        }
        try {
          await api.session.prompt({
            sessionID: own,
            id: `msg_${id}`,
            text,
            metadata: { source: "shelbi", delivery: id },
          });
        } catch (err) {
          answer(id, "failed", String(err?.message ?? err));
          return;
        }
        admitted.add(id);
        answer(id, "admitted");
      } finally {
        try {
          unlinkSync(taken);
        } catch {
          // Already gone.
        }
      }
    };
    let reading = false;
    const readInbox = async () => {
      if (reading) return;
      reading = true;
      try {
        let names;
        try {
          names = readdirSync(inboxDir);
        } catch {
          return;
        }
        const ids = names
          .filter((name) => name.endsWith(".json"))
          .map((name) => name.slice(0, -".json".length))
          .filter((id) => DELIVERY_ID.test(id))
          .sort();
        for (const id of ids) await admit(id);
      } finally {
        reading = false;
      }
    };
    const inboxTimer = setInterval(() => {
      readInbox().catch(() => {});
    }, INBOX_POLL_MS);
    inboxTimer.unref?.();

    await run("session-start.sh");
    await applyTitle();

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
        if (data.sessionID === own) await applyTitle();
        return;
      }
      if (type === "session.renamed") {
        if (data.sessionID === own && title && data.title !== title) await applyTitle();
        return;
      }
      // A form's session is on the form itself.
      const sessionID = data.sessionID ?? data.form?.sessionID;
      if (!sessionID || !(await ours(sessionID))) return;
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
      if (!type.startsWith("session.")) return;
      const child = sessionID !== own;
      if (type === "session.execution.started") {
        if (!child && !titled) await applyTitle();
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
      clearInterval(inboxTimer);
      release();
    };
  },
};
