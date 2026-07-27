// IO shell: node main.mjs record|back|forward
// Invoked by Herdr as a fresh process per focus event / keypress; all
// continuity lives in $HERDR_PLUGIN_STATE_DIR/history.json behind a lock file.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import * as h from "./history.mjs";

const LOCK_STALE_MS = 5000;
const LOCK_RETRY_MS = 10;
const LOCK_ACQUIRE_BUDGET_MS = 3000;
const SOCKET_TIMEOUT_MS = 500;
const JUMP_BUDGET_MS = 1000;

const stateDir = process.env.HERDR_PLUGIN_STATE_DIR;
const socketPath = process.env.HERDR_SOCKET_PATH;
const herdrBin = process.env.HERDR_BIN_PATH;
if (!stateDir || !socketPath || !herdrBin) {
  console.error("herdr-history must run under Herdr (missing HERDR_* env)");
  process.exit(1);
}
const stateFile = path.join(stateDir, "history.json");
const lockFile = path.join(stateDir, "lock");
let lockHeld = false;
process.on("exit", releaseLock);

const mode = process.argv[2];
const step = { back: -1, forward: 1 }[mode];
if (mode !== "record" && step === undefined) {
  console.error(`usage: main.mjs record|back|forward (got: ${mode})`);
  process.exit(1);
}

// The lock is held for the entire run, socket call included: the echo record
// spawned by our own pane.focus must not read state before the cursor lands.
if (!acquireLock()) {
  console.error("lock acquire timed out; dropping invocation");
  process.exit(0);
}

let state = h.expireEchoes(h.loadState(loadRaw(), fingerprint()), Date.now());

if (mode === "record") {
  const paneId = eventPaneId();
  if (paneId) save(h.record(state, paneId));
  process.exit(0);
}

const deadline = Date.now() + JUMP_BUDGET_MS;
// Each failed attempt prunes a dead pane, so the loop is bounded by history
// length; the deadline only caps pathologically slow socket calls.
while (Date.now() < deadline) {
  const plan = h.planJump(state, step);
  if (!plan) {
    notify(`history: at ${step < 0 ? "oldest" : "newest"}`);
    break;
  }
  state = h.pushEcho(state, plan.target, Date.now());
  save(state);
  try {
    const resp = await focusPane(plan.target);
    if (resp.error) throw new Error(resp.error.code ?? "focus failed");
    save(h.onFocusOk(state, plan.index));
    process.exit(0);
  } catch {
    state = h.onFocusFailed(state, plan.index);
    save(state);
  }
}

function acquireLock() {
  const deadline = Date.now() + LOCK_ACQUIRE_BUDGET_MS;
  while (Date.now() < deadline) {
    try {
      fs.closeSync(fs.openSync(lockFile, "wx"));
      lockHeld = true;
      return true;
    } catch (err) {
      if (err.code !== "EEXIST") throw err;
      const stat = fs.statSync(lockFile, { throwIfNoEntry: false });
      if (stat && Date.now() - stat.mtimeMs > LOCK_STALE_MS) {
        // ponytail: unlink-then-retry can double-break with a concurrent
        // breaker; worst case is one lost history entry, never a corrupt file.
        fs.rmSync(lockFile, { force: true });
        continue;
      }
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, LOCK_RETRY_MS);
    }
  }
  return false;
}

function loadRaw() {
  try {
    return fs.readFileSync(stateFile, "utf8");
  } catch {
    return "";
  }
}

// Pane ids recycle across server restarts (fresh sessions number from 1), and
// a recycled hit would focus the wrong pane with no error to prune on — so the
// history resets whenever the server socket identity changes.
function fingerprint() {
  const st = fs.statSync(socketPath);
  return `${st.dev}:${st.ino}:${Math.round(st.birthtimeMs)}`;
}

function eventPaneId() {
  try {
    return JSON.parse(process.env.HERDR_PLUGIN_EVENT_JSON).data.pane_id;
  } catch {
    return null;
  }
}

function save(state) {
  const tmp = `${stateFile}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(state));
  fs.renameSync(tmp, stateFile);
}

function focusPane(paneId) {
  return new Promise((resolve, reject) => {
    const sock = net.createConnection(socketPath);
    let buf = "";
    sock.setEncoding("utf8");
    sock.setTimeout(SOCKET_TIMEOUT_MS, () => sock.destroy(new Error("herdr socket timeout")));
    sock.once("connect", () => {
      sock.write(`${JSON.stringify({ id: "r1", method: "pane.focus", params: { pane_id: paneId } })}\n`);
    });
    sock.on("data", (chunk) => {
      buf += chunk;
      const nl = buf.indexOf("\n");
      if (nl < 0) return;
      sock.destroy();
      try {
        resolve(JSON.parse(buf.slice(0, nl)));
      } catch (err) {
        reject(err);
      }
    });
    sock.once("error", reject);
  });
}

function notify(title) {
  try {
    execFileSync(herdrBin, ["notification", "show", title]);
  } catch {
    // toast is best-effort
  }
}

function releaseLock() {
  if (lockHeld) fs.rmSync(lockFile, { force: true });
}
