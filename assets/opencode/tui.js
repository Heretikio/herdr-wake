// herdr-wake v__HERDR_WAKE_VERSION__: OpenCode TUI plugin. `herdr-wake install` writes this file and overwrites it on upgrade.
//
// Tells herdr-wake which OpenCode session this herdr-core terminal shows, so the
// session can be resumed after a reboot, and tells it when the person quits on
// purpose. It runs `herdr-wake hook opencode <event>` with one JSON object on stdin:
//
//   start  {id, cwd, argv, pid, major}  whenever the root session shown changes.
//   end    {id, signal}                 when the TUI exits (code 0, or after a signal).
//
// One module for both lines, loaded in the terminal's own TUI process (which has
// HC_TERMINAL_ID), never in v2's shared background service (which cannot tell
// terminals apart):
//   v2 (2.x)  CFG/herdr-wake/tui.js, listed in cli.json "plugins"; calls setup(api).
//   v1 (1.x)  CFG/herdr-wake-tui.js, listed in tui.jsonc "plugin"; calls tui(api).
// v1 rejects a module that also has a backend entry, so there is none.
//
// There is no route-change event, so the route is polled every 500 ms on an
// unref'd timer. A sub-agent's child session maps to its root.
//
// OpenCode exits 0 when the person quits; signal exit codes have drifted between
// releases. A SIGHUP/SIGTERM listener put ahead of OpenCode's own sees the signal
// first, so `signal` is true for a closed terminal, a core stop or a shutdown.
// A crash (another exit code without a signal) or SIGKILL reports nothing.
//
// It does nothing outside a herdr-core terminal (no HC_TERMINAL_ID) and never
// throws into OpenCode.

import { spawn, spawnSync } from "node:child_process";

const BIN = "__HERDR_WAKE_BIN__";
const POLL_MS = 500;

// One state per process, shared by both entry points and by a reloaded copy.
const STATE_KEY = Symbol.for("herdr-wake.opencode");
const S = (globalThis[STATE_KEY] ??= { recorded: undefined, signaled: false, armed: false });

function start(fields) {
  try {
    const child = spawn(BIN, ["hook", "opencode", "start"], {
      detached: true,
      stdio: ["pipe", "ignore", "ignore"],
      windowsHide: true,
    });
    child.on("error", () => {});
    child.stdin?.on("error", () => {});
    child.stdin?.end(JSON.stringify(fields));
    child.unref();
  } catch {
    // herdr-wake missing or not runnable: OpenCode goes on as if it were not installed.
  }
}

function end(fields) {
  try {
    // Synchronous: nothing asynchronous runs after the "exit" event.
    spawnSync(BIN, ["hook", "opencode", "end"], {
      input: JSON.stringify(fields),
      stdio: ["pipe", "ignore", "ignore"],
      timeout: 5000,
      windowsHide: true,
    });
  } catch {
    // Same as above.
  }
}

// Signal listeners and the exit hook, once per process.
function arm() {
  if (S.armed) return;
  S.armed = true;
  for (const signal of ["SIGHUP", "SIGTERM"]) {
    const mark = () => {
      S.signaled = true;
      // Alone, this listener would stop the signal from ending the process: end it
      // the default way.
      if (process.listenerCount(signal) === 1) {
        process.removeListener(signal, mark);
        process.kill(process.pid, signal);
      }
    };
    process.prependListener(signal, mark);
  }
  process.on("exit", (code) => {
    if (!S.recorded) return;
    if (code !== 0 && !S.signaled) return; // a crash: leave it resumable
    end({ id: S.recorded, signal: S.signaled });
  });
}

// Polls `current()` (a {id, cwd} for the root session shown, or undefined) and
// reports each new id once. Returns the function that stops polling.
function watch(major, current) {
  arm();
  const tick = () => {
    try {
      const found = current();
      if (!found || typeof found.id !== "string" || !found.id || found.id === S.recorded) return;
      S.recorded = found.id;
      start({
        id: found.id,
        cwd: found.cwd || process.cwd(),
        // argv[0] stands for the program; herdr-wake reads the flags after it.
        argv: ["opencode", ...process.argv.slice(2)],
        pid: process.pid,
        major,
      });
    } catch {
      // Never in OpenCode's way; try again on the next tick.
    }
  };
  tick();
  const timer = setInterval(tick, POLL_MS);
  timer.unref?.();
  return () => clearInterval(timer);
}

// Follows parentID up to the root session. Returns the root's info, or undefined
// while a session on the way is not loaded yet.
function rootOf(id, get) {
  let info = get(id);
  for (let i = 0; info?.parentID && i < 32; i++) {
    id = info.parentID;
    info = get(id);
  }
  return info && !info.parentID ? { id, info } : undefined;
}

// v2: api.ui.router.current() = {type: "session", sessionID}; api.data.session.root(id);
// api.data.session.get(id) = {parentID?, location: {directory}}.
function v2(api) {
  return watch(2, () => {
    const route = api.ui.router.current();
    if (route?.type !== "session" || !route.sessionID) return undefined;
    const top = api.data.session.root?.(route.sessionID) || route.sessionID;
    const found = rootOf(top, (id) => api.data.session.get(id));
    return found && { id: found.id, cwd: found.info.location?.directory };
  });
}

// v1: api.route.current = {name: "session", params: {sessionID}};
// api.state.session.get(id) = {parentID?, directory}.
function v1(api) {
  return watch(1, () => {
    const route = api.route.current;
    const id = route?.name === "session" ? route.params?.sessionID : undefined;
    if (!id) return undefined;
    const found = rootOf(id, (sid) => api.state.session.get(sid));
    return found && { id: found.id, cwd: found.info.directory };
  });
}

export default {
  id: "herdr-wake",
  setup(api) {
    if (!process.env.HC_TERMINAL_ID) return;
    try {
      return v2(api);
    } catch {
      // Never in OpenCode's way.
    }
  },
  async tui(api) {
    if (!process.env.HC_TERMINAL_ID) return;
    try {
      const stop = v1(api);
      api.lifecycle?.onDispose?.(stop);
    } catch {
      // Never in OpenCode's way.
    }
  },
};
