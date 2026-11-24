// Shelbi's OpenCode plugin. Shelbi writes it to
// `.shelbi/opencode/plugins/shelbi/index.js` in every workspace worktree it
// launches OpenCode in, names that directory in the launch's config layer
// (`plugins` in OPENCODE_CONFIG_CONTENT), and rewrites it on each launch, so
// edits here don't last. It is OpenCode's half of the hook contract Claude
// Code gets from `.claude/settings.local.json`: it runs the same
// runner-neutral scripts under `.shelbi/hooks/` on OpenCode's session events.
//
// - The pane's state (working, idle, background, blocked, paused) goes to the
//   hub as a `runner-state` report over $SHELBI_HUB_ADDR, the address every
//   Shelbi pane carries (forwarded on an SSH host too). The hub records the
//   latest report and the poller reads it instead of the pane title, so the
//   plugin never writes to the pane's terminal. A report says why: `blocked`
//   names the ask (`permission`, `question`), `paused` the provider's error
//   (`provider.free-limit`, `provider.rate-limit`, ...) and, for a scheduled
//   retry, when it runs (`until`). Which event means which state is the
//   worker's `handlers` table below.
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
// A pane Shelbi didn't launch (no SHELBI_OPENCODE_SESSION) is left alone.
import { execFile, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import net from "node:net";
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
const PLUGIN_VERSION = 4;
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
// How long one hub report may take, and the pause before its one retry.
const HUB_TIMEOUT_MS = 3000;
const HUB_RETRY_MS = 500;

// OpenCode's error types (`session.execution.failed`, `session.retry.scheduled`)
// that mean a provider stopped the session on a limit rather than broke. Any
// other `provider.*` type naming a limit or quota counts too, so a renamed or
// new limit still reads as paused.
const LIMIT_ERRORS = new Set([
  "provider.free-limit",
  "provider.go-limit",
  "provider.rate-limit",
  "provider.quota",
  "provider.budget-limit",
  "provider.model-budget-limit",
  "provider.chatgpt-limit",
  "provider.chatgpt-workspace-limit",
]);
const isLimitError = (type) =>
  typeof type === "string" &&
  (LIMIT_ERRORS.has(type) || (type.startsWith("provider.") && /limit|quota/.test(type)));

// Where the hub listens: `unix:<path>` or `tcp:<host>:<port>` in
// SHELBI_HUB_ADDR, else the legacy SHELBI_HUB_SOCK path. Null when neither is
// set (a pane Shelbi didn't wire to a hub).
const hubTarget = (env) => {
  const addr = env.SHELBI_HUB_ADDR || (env.SHELBI_HUB_SOCK ? `unix:${env.SHELBI_HUB_SOCK}` : "");
  if (addr.startsWith("unix:")) return { path: addr.slice(5) };
  if (addr.startsWith("tcp:")) {
    const hostPort = addr.slice(4);
    const at = hostPort.lastIndexOf(":");
    const port = Number(hostPort.slice(at + 1));
    if (at > 0 && Number.isInteger(port)) return { host: hostPort.slice(0, at), port };
  }
  return null;
};

// Send one JSON line to the hub and resolve whether it answered `ok`. The
// write side is closed after the line, so a report the hub rejects (it sends
// no `ok`) ends with the connection instead of waiting out the timeout.
const sendHub = (target, body) =>
  new Promise((resolve) => {
    let reply = "";
    let settled = false;
    const done = (ok) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      resolve(ok);
    };
    const socket = net.createConnection(target, () => socket.end(`${JSON.stringify(body)}\n`));
    socket.setTimeout(HUB_TIMEOUT_MS, () => done(false));
    socket.on("data", (chunk) => {
      reply += chunk;
      if (reply.includes("\n")) done(reply.trim() === "ok");
    });
    socket.on("end", () => done(reply.trim() === "ok"));
    socket.on("error", () => done(false));
  });

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
    const own = process.env.SHELBI_OPENCODE_SESSION;
    const title = process.env.SHELBI_OPENCODE_TITLE;
    const orchestrator = process.env.SHELBI_OPENCODE_ROLE === ORCHESTRATOR;
    const root = api.location?.directory || process.cwd();
    if (!own) return;
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

    // Report the pane's state to the hub. Reports go out one at a time, in
    // order, and only when something changed. A send that gets no `ok` is
    // retried once; past that the hub reconciles from what it sees.
    const hub = hubTarget(process.env);
    const project = process.env.PROJECT;
    const workspace = process.env.SHELBI_WORKSPACE;
    const task = process.env.TASK_ID;
    let reported = "";
    let outbox = Promise.resolve();
    const report = (state, reason, until) => {
      const key = `${state}|${reason ?? ""}|${until ?? ""}`;
      if (key === reported) return outbox;
      reported = key;
      if (!hub || !project || !workspace) return outbox;
      const body = { verb: "runner-state", project, workspace, state };
      if (task) body.task_id = task;
      if (reason) body.reason = reason;
      if (until) body.until = until;
      outbox = outbox.then(async () => {
        if (await sendHub(hub, body)) return;
        await sleep(HUB_RETRY_MS);
        await sendHub(hub, body);
      });
      return outbox;
    };
    let current = "";
    const mark = (state, reason, until) => {
      current = state;
      return report(state, reason, until);
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

    // The pane's turn ended. New messages start the next one; otherwise the
    // pane is paused on a provider limit, waiting on a subagent it started,
    // or idle at its prompt.
    const turnEnded = async (sessionID, paused) => {
      if (await drain(sessionID)) return;
      drainedSize = messageLog ? logSize() : 0;
      idleSession = sessionID;
      if (paused) return mark("paused", paused);
      return mark(runningChildren.size > 0 ? "background" : "idle");
    };
    const busy = () => {
      idleSession = null;
      return mark("working");
    };
    const ended = (child, sessionID, paused) => {
      if (child) {
        runningChildren.delete(sessionID);
        return;
      }
      return turnEnded(sessionID, paused);
    };

    // What each OpenCode event means for the pane, keyed by event type. Each
    // handler gets the event's data, whether it came from a subagent, and
    // its session id. Only the pane's session and its subagents reach here.
    // Event names and shapes are OpenCode 2.4's (recorded in
    // `tests/fixtures/opencode/events-2.4.1.sse`, which a contract test
    // replays through this table).
    const handlers = {
      // An ask, from the session or a subagent, waits on a person.
      "permission.asked": () => mark("blocked", "permission"),
      "form.created": () => mark("blocked", "question"),
      // Answered either way, the turn carries on. (A rejected ask is a
      // `permission.replied` with `reply: "reject"`.)
      "permission.replied": () => current === "blocked" && mark("working"),
      "form.replied": () => current === "blocked" && mark("working"),
      "form.cancelled": () => current === "blocked" && mark("working"),
      "session.execution.started": (_data, child, sessionID) => {
        if (child) {
          runningChildren.add(sessionID);
          return;
        }
        if (!titled) applyTitle();
        return busy();
      },
      "session.execution.succeeded": (_data, child, sessionID) => ended(child, sessionID),
      "session.execution.interrupted": (_data, child, sessionID) => ended(child, sessionID),
      // A failed turn on a provider limit is paused, not idle: nobody needs
      // to answer anything, the limit has to lift.
      "session.execution.failed": (data, child, sessionID) => {
        const type = data.error?.type;
        return ended(child, sessionID, isLimitError(type) ? type : null);
      },
      // OpenCode waits out a retryable provider error by itself (a rate
      // limit, an overloaded provider) and says until when.
      "session.retry.scheduled": (data, child) => {
        if (child) return;
        idleSession = null;
        const at = Number(data.at);
        const until = Number.isFinite(at) && at > 0 ? new Date(at).toISOString() : undefined;
        return mark("paused", data.error?.type || "retry", until);
      },
      // Work moving again: a retried step, new input, a tool call.
      "session.step.started": (_data, child) => !child && busy(),
      "session.inbox.enqueued": (_data, child) => !child && busy(),
      "session.tool.called": (_data, child) => !child && busy(),
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
      const handler = handlers[type];
      if (!handler) return;
      // A form's session is on the form itself.
      const sessionID = data.sessionID ?? data.form?.sessionID;
      if (!sessionID || !(await ours(sessionID))) return;
      await handler(data, sessionID !== own, sessionID);
    };

    const controller = new AbortController();
    (async () => {
      for await (const event of api.event.subscribe({ signal: controller.signal })) {
        await handle(event).catch(() => {});
      }
    })().catch(() => {});
    return async () => {
      controller.abort();
      if (timer) clearInterval(timer);
      clearInterval(inboxTimer);
      release();
      await outbox.catch(() => {});
    };
  },
};
