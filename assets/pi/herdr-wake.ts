// herdr-wake v__HERDR_WAKE_VERSION__: Pi extension. `herdr-wake install` writes this file and overwrites it on upgrade.
//
// Tells herdr-wake which Pi session runs in this herdr-core terminal, so the
// session can be resumed after a reboot, and tells it when the person quits on
// purpose. It runs `herdr-wake hook pi <event>` with one JSON object on stdin:
//
//   start  {reason, id, file, cwd, argv, pid}  on session_start (once the session
//          file exists) and on agent_start (the first prompt writes the file).
//   end    {id, signal}                        on session_shutdown with reason "quit".
//
// Pi reports "quit" for Ctrl+C, Ctrl+D and /quit, and also for SIGHUP and SIGTERM
// (a closed terminal, a core stop, a shutdown). A listener put ahead of Pi's own
// sees those signals first, so `signal` is true when the quit came from one.
// SIGKILL, a crash or a power loss report nothing: the session stays resumable.
//
// It does nothing outside a herdr-core terminal (no HC_TERMINAL_ID) or outside the
// interactive TUI (print, json and rpc modes, and sub-agents run as `pi -p`), and it
// never throws into Pi.

import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";

const BIN = "__HERDR_WAKE_BIN__";

// One state per process: Pi runs this factory again on /reload, and the signal
// listeners must not pile up.
const STATE_KEY = Symbol.for("herdr-wake.pi");
type State = { recorded?: string; signaled: boolean; listening: boolean };
const S: State = ((globalThis as any)[STATE_KEY] ??= { signaled: false, listening: false });

function start(fields: Record<string, unknown>): void {
  try {
    const child = spawn(BIN, ["hook", "pi", "start"], {
      detached: true,
      stdio: ["pipe", "ignore", "ignore"],
      windowsHide: true,
    });
    child.on("error", () => {});
    child.stdin?.on("error", () => {});
    child.stdin?.end(JSON.stringify(fields));
    child.unref();
  } catch {
    // herdr-wake missing or not runnable: Pi goes on as if it were not installed.
  }
}

function end(fields: Record<string, unknown>): void {
  try {
    // Synchronous: Pi exits right after the shutdown handlers return.
    spawnSync(BIN, ["hook", "pi", "end"], {
      input: JSON.stringify(fields),
      stdio: ["pipe", "ignore", "ignore"],
      timeout: 5000,
      windowsHide: true,
    });
  } catch {
    // Same as above.
  }
}

// Marks SIGHUP and SIGTERM. Pi installs its own handlers with prependListener too,
// and emits session_shutdown only after an await, so every listener of the signal
// has run by the time the shutdown handler below reads `signaled`.
function listen(): void {
  if (S.listening) return;
  S.listening = true;
  for (const signal of ["SIGHUP", "SIGTERM"] as const) {
    const mark = () => {
      S.signaled = true;
      // Alone, this listener would stop the signal from ending the process
      // (Pi removes its handlers on a crash): end it the default way.
      if (process.listenerCount(signal) === 1) {
        process.removeListener(signal, mark);
        process.kill(process.pid, signal);
      }
    };
    process.prependListener(signal, mark);
  }
}

// Records the session shown in this terminal once per session id. With
// `onlyIfWritten`, a session whose file does not exist yet waits for its first
// prompt (a session nobody typed into is not worth resuming).
function note(reason: string, ctx: any, onlyIfWritten: boolean): void {
  try {
    if (ctx?.mode !== "tui") return;
    const sm = ctx.sessionManager;
    const id = sm?.getSessionId?.();
    const file = sm?.getSessionFile?.();
    // --no-session has no file and cannot be resumed.
    if (typeof id !== "string" || !id || typeof file !== "string" || !file) return;
    if (id === S.recorded) return;
    if (onlyIfWritten && !existsSync(file)) return;
    S.recorded = id;
    start({
      reason,
      id,
      file,
      cwd: ctx.cwd ?? sm.getCwd?.() ?? process.cwd(),
      // argv[0] stands for the program; herdr-wake reads the flags after it.
      argv: ["pi", ...process.argv.slice(2)],
      pid: process.pid,
    });
  } catch {
    // Never in Pi's way.
  }
}

export default function (pi: any) {
  if (!process.env.HC_TERMINAL_ID) return;

  pi.on("session_start", (event: any, ctx: any) => {
    try {
      if (ctx?.mode !== "tui") return;
      listen();
      note(String(event?.reason ?? "startup"), ctx, true);
    } catch {
      // Never in Pi's way.
    }
  });

  pi.on("agent_start", (_event: any, ctx: any) => note("prompt", ctx, false));

  // "new", "resume", "fork" and "reload" go on in another session, whose
  // session_start records it. Only "quit" ends the record.
  pi.on("session_shutdown", (event: any, ctx: any) => {
    try {
      if (event?.reason !== "quit" || ctx?.mode !== "tui" || !S.recorded) return;
      end({ id: S.recorded, signal: S.signaled });
    } catch {
      // Never in Pi's way.
    }
  });
}
