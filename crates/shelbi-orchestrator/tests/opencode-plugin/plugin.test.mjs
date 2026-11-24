// Unit tests for Shelbi's OpenCode plugin (`src/opencode_plugin.js`), run
// with Node's own test runner against a fake OpenCode plugin context:
//
//   node --test crates/shelbi-orchestrator/tests/opencode-plugin/
//
// The fake `ctx` has the parts of OpenCode 2.x's plugin API the plugin uses
// (`location.directory`, `session.get/update/synthetic/prompt` and
// `event.subscribe`), records every session call, and lets a test feed it
// events one by one. A fake hub (a local socket answering `ok`) records the
// `runner-state` reports. Nothing here reaches a real OpenCode, a real hub or
// the user's Shelbi home: every hub variable is cleared before the plugin
// loads, and each test works in its own scratch worktree.
//
// The Rust suite (`workspace/opencode.rs`) also runs the plugin under Node:
// the single-instance lock, the orchestrator's awareness contract, and the
// replay of a recorded OpenCode 2.4.1 event stream. These tests cover the
// worker's side in detail: scoping, titles, state reports, message drains and
// verified sends.

import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import {
  appendFileSync,
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import net from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SOURCE = fileURLToPath(new URL("../../src/opencode_plugin.js", import.meta.url));
const OWN = "ses_shelbi0123456789abcdef0123";
const TITLE = "Shelbi · alpha · fix-it";

// Every variable the plugin reads. Cleared up front so a run from inside a
// Shelbi pane never reports to the live hub.
const ENV_KEYS = [
  "SHELBI_OPENCODE_SESSION",
  "SHELBI_OPENCODE_TITLE",
  "SHELBI_OPENCODE_ROLE",
  "SHELBI_HUB_ADDR",
  "SHELBI_HUB_SOCK",
  "SHELBI_BIN",
  "PROJECT",
  "SHELBI_WORKSPACE",
  "TASK_ID",
];

let plugin;
let scratch;

before(async () => {
  for (const key of ENV_KEYS) delete process.env[key];
  scratch = mkdtempSync(path.join(tmpdir(), "shelbi-ocp-"));
  // The file Shelbi embeds is an ES module named `.js` with no package.json
  // beside it; a `.mjs` copy loads as a module on every Node version.
  const copy = path.join(scratch, "plugin.mjs");
  copyFileSync(SOURCE, copy);
  plugin = (await import(pathToFileURL(copy).href)).default;
  // Each active instance adds an `exit` listener for its lock.
  process.setMaxListeners(0);
});

after(() => {
  rmSync(scratch, { recursive: true, force: true });
});

// Set the pane environment the plugin reads at `setup`, clearing the rest.
const paneEnv = (vars) => {
  for (const key of ENV_KEYS) delete process.env[key];
  Object.assign(process.env, vars);
};

const script = (file, body) => {
  writeFileSync(file, body);
  chmodSync(file, 0o755);
};

// A scratch worktree with the hook scripts a worker's plugin runs.
// `stop.sh` prints (and consumes) `.shelbi/next-batch`, standing in for the
// hub's message drain; both scripts log their runs to `.shelbi/hook.log`.
const worktree = (name, { hooks = true } = {}) => {
  const root = mkdtempSync(path.join(scratch, `${name}-`));
  if (hooks) {
    const dir = path.join(root, ".shelbi/hooks");
    mkdirSync(dir, { recursive: true });
    script(path.join(dir, "session-start.sh"), "#!/bin/sh\necho session-start >> .shelbi/hook.log\n");
    script(
      path.join(dir, "stop.sh"),
      "#!/bin/sh\necho stop >> .shelbi/hook.log\n" +
        "if [ -f .shelbi/next-batch ]; then cat .shelbi/next-batch; rm .shelbi/next-batch; fi\n",
    );
  }
  return root;
};

// A fake OpenCode plugin context. `sessions` maps a session id to what
// `session.get` returns (an unknown id throws, as OpenCode does). `emit`
// queues an event; `settled` resolves once the plugin has handled every
// queued event (its loop asked for the next one).
const fakeCtx = (root, { sessions = {}, prompt } = {}) => {
  const calls = [];
  const queue = [];
  let wake = () => {};
  let waiting = false;
  let waiters = [];
  const idle = () => {
    waiting = true;
    const done = waiters;
    waiters = [];
    for (const resolve of done) resolve();
  };
  const api = {
    location: { directory: root },
    session: {
      get: async ({ sessionID }) => {
        if (sessionID in sessions) return sessions[sessionID];
        throw new Error(`no session ${sessionID}`);
      },
      update: async (input) => {
        calls.push({ call: "update", ...input });
      },
      synthetic: async (input) => {
        calls.push({ call: "synthetic", ...input });
      },
      prompt: async (input) => {
        calls.push({ call: "prompt", ...input });
        if (prompt) return prompt(input);
        return { id: input.id };
      },
    },
    event: {
      subscribe: ({ signal }) => ({
        async *[Symbol.asyncIterator]() {
          while (!signal.aborted) {
            if (queue.length) {
              yield queue.shift();
              continue;
            }
            idle();
            await new Promise((resolve) => {
              wake = resolve;
              signal.addEventListener("abort", resolve, { once: true });
            });
          }
        },
      }),
    },
  };
  const emit = (type, data) => {
    waiting = false;
    queue.push({ type, data });
    wake();
  };
  // Bounded, so a plugin that stops pulling events fails the test instead
  // of hanging the run.
  const settled = (ms = 10000) =>
    new Promise((resolve, reject) => {
      if (waiting && !queue.length) return resolve();
      const timer = setTimeout(() => reject(new Error("the plugin stopped handling events")), ms);
      waiters.push(() => {
        clearTimeout(timer);
        resolve();
      });
    });
  return { api, calls, emit, settled };
};

// A fake hub: records each JSON line and answers `answer(body, n)` (`ok` by
// default). Listens on loopback TCP, or on a Unix socket at `sock`.
const fakeHub = async (t, { sock, answer = () => "ok\n" } = {}) => {
  const reports = [];
  const server = net.createServer((conn) => {
    let buffer = "";
    conn.on("error", () => {});
    conn.on("data", (chunk) => {
      buffer += chunk;
      const end = buffer.indexOf("\n");
      if (end < 0) return;
      const body = JSON.parse(buffer.slice(0, end));
      reports.push(body);
      conn.end(answer(body, reports.length));
    });
  });
  if (sock) await new Promise((resolve) => server.listen(sock, resolve));
  else await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = sock ? `unix:${sock}` : `tcp:127.0.0.1:${server.address().port}`;
  t.after(() => new Promise((resolve) => server.close(resolve)));
  return { reports, addr };
};

// Run the plugin's `setup`, and stop the instance when the test ends, pass or
// fail: a live instance keeps its event loop, timers and lock going, and
// would hold the test run open.
const start = async (t, api) => {
  const stop = await plugin.setup(api);
  if (typeof stop === "function") t.after(() => stop());
  return stop;
};

// `state` or `state:reason`, as a report reads in a sequence.
const summary = (report) => (report.reason ? `${report.state}:${report.reason}` : report.state);

const until = async (what, done, ms = 10000) => {
  const deadline = Date.now() + ms;
  while (!done()) {
    if (Date.now() > deadline) assert.fail(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
};

// A delivery id in the shape Shelbi mints: 12 hex digits, 14 base62.
const deliveryId = (n) => `${n.toString(16).padStart(12, "0")}AbCdEfGhIjKlMn`;

test("stays passive in a pane Shelbi didn't launch, or with no hook scripts", async (t) => {
  const root = worktree("passive");
  paneEnv({});
  assert.equal(await plugin.setup(fakeCtx(root).api), undefined);
  assert.ok(!existsSync(path.join(root, ".shelbi/opencode/plugin.lock")), "no lock without a session");

  const bare = worktree("bare", { hooks: false });
  paneEnv({ SHELBI_OPENCODE_SESSION: OWN });
  assert.equal(await plugin.setup(fakeCtx(bare).api), undefined, "a worker needs the hook scripts");
  assert.ok(!existsSync(path.join(bare, ".shelbi/opencode/plugin.alive")));
});

test("takes the worktree's lock, writes its beacon and runs session-start", async (t) => {
  const root = worktree("beacon");
  paneEnv({ SHELBI_OPENCODE_SESSION: OWN });
  const stop = await start(t, fakeCtx(root).api);
  assert.equal(typeof stop, "function");
  const beacon = JSON.parse(readFileSync(path.join(root, ".shelbi/opencode/plugin.alive"), "utf8"));
  assert.equal(beacon.plugin, "shelbi");
  assert.equal(beacon.pid, process.pid);
  assert.ok(Number.isInteger(beacon.version) && beacon.version > 0);
  assert.equal(readFileSync(path.join(root, ".shelbi/hook.log"), "utf8"), "session-start\n");
  // A second setup for the same worktree (OpenCode may run one per location)
  // stays passive.
  assert.equal(await plugin.setup(fakeCtx(root).api), undefined);
  await stop();
  assert.ok(!existsSync(path.join(root, ".shelbi/opencode/plugin.lock")), "cleanup releases the lock");
});

test("titles its own session and puts the title back when it changes", async (t) => {
  const root = worktree("title");
  paneEnv({ SHELBI_OPENCODE_SESSION: OWN, SHELBI_OPENCODE_TITLE: TITLE });
  const ctx = fakeCtx(root);
  await start(t, ctx.api);
  const updates = () => ctx.calls.filter((c) => c.call === "update");
  assert.deepEqual(updates(), [{ call: "update", sessionID: OWN, title: TITLE }]);

  // OpenCode's title agent renames it: Shelbi's title goes back.
  ctx.emit("session.renamed", { sessionID: OWN, title: "Fix the login bug" });
  // Its own rename echoing back, and another session's rename, change nothing.
  ctx.emit("session.renamed", { sessionID: OWN, title: TITLE });
  ctx.emit("session.renamed", { sessionID: "ses_users_own", title: "Mine" });
  await ctx.settled();
  assert.equal(updates().length, 2);
  assert.deepEqual(updates()[1], { call: "update", sessionID: OWN, title: TITLE });
});

test("reports its own turns and its subagents' asks to the hub, and nothing else", async (t) => {
  const root = worktree("states");
  const hub = await fakeHub(t);
  paneEnv({
    SHELBI_OPENCODE_SESSION: OWN,
    SHELBI_HUB_ADDR: hub.addr,
    PROJECT: "demo",
    SHELBI_WORKSPACE: "alpha",
    TASK_ID: "fix-it",
  });
  const ctx = fakeCtx(root, {
    sessions: {
      ses_child: { parentID: OWN },
      ses_grandchild: { parentID: "ses_child" },
      ses_users_own: {},
    },
  });
  await start(t, ctx.api);

  const events = [
    ["session.execution.started", { sessionID: OWN }],
    // A session the user opened in the same TUI isn't Shelbi's.
    ["session.execution.started", { sessionID: "ses_users_own" }],
    ["permission.asked", { sessionID: "ses_users_own" }],
    // A subagent (and its own subagent) asking blocks the pane.
    ["session.execution.started", { sessionID: "ses_child" }],
    ["permission.asked", { sessionID: "ses_grandchild" }],
    ["permission.replied", { sessionID: "ses_grandchild", reply: "once" }],
    // The pane's turn ends while the subagent still runs: background.
    ["session.execution.succeeded", { sessionID: OWN }],
    // A subagent's end never ends the pane's turn.
    ["session.execution.succeeded", { sessionID: "ses_child" }],
    ["session.execution.started", { sessionID: OWN }],
    // A question form names its session on the form.
    ["form.created", { form: { sessionID: OWN } }],
    ["form.cancelled", { form: { sessionID: OWN } }],
    // A limit OpenCode has no name for yet still reads as a limit.
    ["session.execution.failed", { sessionID: OWN, error: { type: "provider.weekly-quota" } }],
    ["session.execution.started", { sessionID: OWN }],
    // A retry OpenCode scheduled says when it runs.
    ["session.retry.scheduled", { sessionID: OWN, at: Date.UTC(2026, 10, 24, 12, 0, 0), error: { type: "provider.overloaded" } }],
    ["session.step.started", { sessionID: OWN }],
    // A failure that isn't a limit ends the turn idle.
    ["session.execution.failed", { sessionID: OWN, error: { type: "provider.auth" } }],
    // The same state again sends nothing.
    ["session.execution.interrupted", { sessionID: OWN }],
  ];
  for (const [type, data] of events) ctx.emit(type, data);
  await ctx.settled();

  assert.deepEqual(hub.reports.map(summary), [
    "working",
    "blocked:permission",
    "working",
    "background",
    "working",
    "blocked:question",
    "working",
    "paused:provider.weekly-quota",
    "working",
    "paused:provider.overloaded",
    "working",
    "idle",
  ]);
  for (const report of hub.reports) {
    assert.equal(report.verb, "runner-state");
    assert.equal(report.project, "demo");
    assert.equal(report.workspace, "alpha");
    assert.equal(report.task_id, "fix-it");
  }
  assert.equal(hub.reports[9].until, "2026-11-24T12:00:00.000Z");
  assert.equal(hub.reports[7].until, undefined);
  // Every turn end ran the message drain.
  const log = readFileSync(path.join(root, ".shelbi/hook.log"), "utf8");
  assert.equal(log.match(/^stop$/gm).length, 4);
});

test("an ask answered while nothing is blocked leaves the state alone", async (t) => {
  const root = worktree("replied");
  const hub = await fakeHub(t);
  paneEnv({ SHELBI_OPENCODE_SESSION: OWN, SHELBI_HUB_ADDR: hub.addr, PROJECT: "demo", SHELBI_WORKSPACE: "alpha" });
  const ctx = fakeCtx(root);
  await start(t, ctx.api);
  ctx.emit("session.execution.started", { sessionID: OWN });
  ctx.emit("session.execution.succeeded", { sessionID: OWN });
  ctx.emit("permission.replied", { sessionID: OWN, reply: "reject" });
  await ctx.settled();
  assert.deepEqual(hub.reports.map(summary), ["working", "idle"]);
  // No TASK_ID in the pane: the report carries none.
  assert.ok(!("task_id" in hub.reports[0]));
});

test("a turn's end delivers the messages the hub has waiting", async (t) => {
  const root = worktree("drain");
  const hub = await fakeHub(t);
  paneEnv({ SHELBI_OPENCODE_SESSION: OWN, SHELBI_HUB_ADDR: hub.addr, PROJECT: "demo", SHELBI_WORKSPACE: "alpha" });
  const ctx = fakeCtx(root);
  await start(t, ctx.api);

  ctx.emit("session.execution.started", { sessionID: OWN });
  await ctx.settled();
  writeFileSync(path.join(root, ".shelbi/next-batch"), "[shelbi] message from the orchestrator: rebase first\n");
  ctx.emit("session.execution.succeeded", { sessionID: OWN });
  await ctx.settled();
  // The batch goes in as a synthetic message (which starts the next turn),
  // so the pane never reads as idle.
  const synthetic = ctx.calls.filter((c) => c.call === "synthetic");
  assert.equal(synthetic.length, 1);
  assert.equal(synthetic[0].sessionID, OWN);
  assert.equal(synthetic[0].description, "Shelbi messages");
  assert.equal(synthetic[0].text, "[shelbi] message from the orchestrator: rebase first");
  assert.deepEqual(synthetic[0].metadata, { source: "shelbi" });
  assert.deepEqual(hub.reports.map(summary), ["working"]);

  // Nothing waiting: idle.
  ctx.emit("session.execution.succeeded", { sessionID: OWN });
  await ctx.settled();
  assert.deepEqual(hub.reports.map(summary), ["working", "idle"]);
  assert.equal(ctx.calls.filter((c) => c.call === "synthetic").length, 1);
});

test("while idle, a message the hub appends to the task's log is delivered", async (t) => {
  const root = worktree("idle-drain");
  const hub = await fakeHub(t);
  const log = path.join(root, ".shelbi/messages/fix-it.log");
  mkdirSync(path.dirname(log), { recursive: true });
  writeFileSync(log, "");
  paneEnv({
    SHELBI_OPENCODE_SESSION: OWN,
    SHELBI_HUB_ADDR: hub.addr,
    PROJECT: "demo",
    SHELBI_WORKSPACE: "alpha",
    TASK_ID: "fix-it",
  });
  const ctx = fakeCtx(root);
  await start(t, ctx.api);
  ctx.emit("session.execution.started", { sessionID: OWN });
  ctx.emit("session.execution.succeeded", { sessionID: OWN });
  await ctx.settled();
  assert.deepEqual(hub.reports.map(summary), ["working", "idle"]);

  writeFileSync(path.join(root, ".shelbi/next-batch"), "[shelbi] message: are you stuck?\n");
  appendFileSync(log, "2026-11-24T12:00:00Z orchestrator: are you stuck?\n");
  // The idle check runs every 5 s.
  await until("the idle drain", () => ctx.calls.some((c) => c.call === "synthetic"), 12000);
  const synthetic = ctx.calls.find((c) => c.call === "synthetic");
  assert.equal(synthetic.text, "[shelbi] message: are you stuck?");
  await until("the working report", () => hub.reports.length === 3);
  assert.equal(summary(hub.reports[2]), "working");
});

test("verified sends: each inbox entry is admitted once, in order, and answered", async (t) => {
  const root = worktree("inbox");
  paneEnv({ SHELBI_OPENCODE_SESSION: OWN });
  const ctx = fakeCtx(root, {
    prompt: (input) => {
      if (input.text === "refuse me") throw new Error("session is gone");
      return { id: input.id };
    },
  });
  await start(t, ctx.api);
  const inbox = path.join(root, ".shelbi/opencode/inbox");
  const ackDir = path.join(root, ".shelbi/opencode/ack");
  mkdirSync(inbox, { recursive: true });
  const drop = (id, body) => writeFileSync(path.join(inbox, `${id}.json`), JSON.stringify(body));
  const ack = (id) => {
    try {
      return JSON.parse(readFileSync(path.join(ackDir, `${id}.json`), "utf8"));
    } catch {
      return null;
    }
  };
  const prompts = () => ctx.calls.filter((c) => c.call === "prompt");

  // Two entries at once go in id (send) order.
  const [first, second] = [deliveryId(0x19a1), deliveryId(0x19a2)];
  drop(second, { id: second, text: "second" });
  drop(first, { id: first, text: "first" });
  await until("both acks", () => ack(first) && ack(second));
  assert.deepEqual(ack(first), { id: first, status: "admitted" });
  assert.deepEqual(ack(second), { id: second, status: "admitted" });
  assert.deepEqual(
    prompts().map((p) => [p.id, p.text]),
    [
      [`msg_${first}`, "first"],
      [`msg_${second}`, "second"],
    ],
  );
  assert.equal(prompts()[0].sessionID, OWN);
  assert.deepEqual(prompts()[0].metadata, { source: "shelbi", delivery: first });
  // The claimed files are gone.
  assert.ok(!existsSync(path.join(inbox, `${first}.json`)) && !existsSync(path.join(inbox, `${first}.taken`)));

  // The same id dropped again is answered as a duplicate, not admitted.
  unlinkSync(path.join(ackDir, `${first}.json`));
  drop(first, { id: first, text: "first" });
  await until("the duplicate's ack", () => ack(first));
  assert.deepEqual(ack(first), { id: first, status: "duplicate" });
  assert.equal(prompts().length, 2);

  // Entries the plugin can't admit are answered `failed`, with why.
  const [mismatch, empty, refused] = [deliveryId(0x19a3), deliveryId(0x19a4), deliveryId(0x19a5)];
  drop(mismatch, { id: deliveryId(0x1), text: "not mine" });
  drop(empty, { id: empty, text: "   " });
  drop(refused, { id: refused, text: "refuse me" });
  await until("the failure acks", () => ack(mismatch) && ack(empty) && ack(refused));
  assert.equal(ack(mismatch).status, "failed");
  assert.match(ack(mismatch).error, /unreadable entry/);
  assert.deepEqual(ack(empty), { id: empty, status: "failed", error: "empty message" });
  assert.deepEqual(ack(refused), { id: refused, status: "failed", error: "session is gone" });

  // A file that isn't a delivery is left where it is.
  writeFileSync(path.join(inbox, "notes.json"), "{}");
  await new Promise((resolve) => setTimeout(resolve, 600));
  assert.ok(existsSync(path.join(inbox, "notes.json")));
  assert.ok(!existsSync(path.join(ackDir, "notes.json")));
});

test("reports over a Unix socket, retrying once when the hub doesn't answer ok", async (t) => {
  const root = worktree("unix");
  // Short: a Unix socket path has a ~100 byte limit on macOS.
  const sock = path.join(tmpdir(), `shb-ocp-${process.pid}.sock`);
  rmSync(sock, { force: true });
  const hub = await fakeHub(t, { sock, answer: (_body, n) => (n === 1 ? "" : "ok\n") });
  // The legacy socket variable alone is enough.
  paneEnv({ SHELBI_OPENCODE_SESSION: OWN, SHELBI_HUB_SOCK: sock, PROJECT: "demo", SHELBI_WORKSPACE: "alpha" });
  const ctx = fakeCtx(root);
  await start(t, ctx.api);
  ctx.emit("session.execution.started", { sessionID: OWN });
  ctx.emit("session.execution.succeeded", { sessionID: OWN });
  await ctx.settled();
  assert.deepEqual(hub.reports.map(summary), ["working", "working", "idle"]);
  rmSync(sock, { force: true });
});
