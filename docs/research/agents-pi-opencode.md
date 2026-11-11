# Pi and OpenCode adapters

Researched 2026-11-10. Sources:

- **[P]** Pi `@earendil-works/pi-coding-agent` 1.5.2 npm tarball (github.com/earendil-works/pi).
- **[O2]** OpenCode source, github.com/anomalyco/opencode `dev` @ c4bf172 (`packages/cli` 2.2.7). **[O1]** same repo, tag v1.19.5.
- Clones in `scratchpad/repos/`: **[H]** herdrdev_herdr @ ee5bfb5, **[R]** guillaume86_herdr-recover @ f1d6b76, **[V]** dmazlum_herdr-revive, **[T]** thuanlm215_herdr-agent-resume, **[X]** pattont_RexAgentRestore, **[M]** manastalukdar_herdr-agent-resume-plus.
- **[I]** github.com/herdrdev/herdr/issues/5921.

## Pi

### 1. Discovery

A TS/JS module that default-exports `(pi) => void` is auto-loaded from `~/.pi/agent/extensions/` [P docs/extensions.md:127-133]. `PI_CODING_AGENT_DIR` moves that folder [P docs/environment-variables.md:81]. Install: write `extensions/herdr-wake.ts` (version marker), overwrite on upgrade, trash on uninstall [H src/integration/targets.rs:80-87].

```ts
import { spawn } from "node:child_process";
export default function (pi: any) {
  if (!process.env.HC_TERMINAL_ID) return;
  const note = (reason: string, ctx: any) => {
    if (ctx?.mode !== "tui") return;                     // print, json, rpc
    const sm = ctx.sessionManager, c = spawn("herdr-wake", ["hook", "pi"],
      { detached: true, stdio: ["pipe", "ignore", "ignore"] });
    c.on("error", () => {}); c.unref();
    c.stdin.end(JSON.stringify({ reason, id: sm.getSessionId(),
      file: sm.getSessionFile(), cwd: ctx.cwd, argv: process.argv.slice(2) }));
  };
  pi.on("session_start", (e: any, ctx: any) => note(e.reason, ctx));
  pi.on("agent_start", (_e: any, ctx: any) => note("prompt", ctx));
}
```

- **Mode gate.** `ctx.mode` is `tui|rpc|json|print` [P extensions.md:1171-1175]. Check it rather than `hasUI`, because RPC mode reports `hasUI=true` [H assets/pi/herdr-agent-state.ts:230-234].
- **Session reads.** `getSessionId()` and `getSessionFile()` are on `ctx.sessionManager` [P :1205].
- **Late file.** A new session's file is only written at the first user message [P dist/core/session-manager.js:786-801], hence the second hook on `agent_start` [T scripts/pi_extension.js:19-44].
- **CLI access.** Node: `node:child_process` or `pi.exec()` [P :1897]; the terminal's env, `HC_TERMINAL_ID` included, is inherited.
- **Flags.** Read `process.argv`; Pi renames its process [V mods/revive/lib/core.js:161].

### 2. Id changes

`/new`, `/resume`, `/fork`, `/clone` and `/reload` fire `session_shutdown`, then `session_start` with reason `new|resume|fork|reload` and `previousSessionFile` [P extensions.md:376-393,512,529]. Compaction and `/tree` keep the same file [P :395-400].

### 3. Skip

`mode !== "tui"` covers `-p`, `--mode json|rpc`, and redirected stdio [P docs/cli.md:32-50]. It also covers extension sub-agents, which run `pi -p` [V mods/revive/pi/revive.ts:18]. `--no-session` has no file.

### 4. End

`session_shutdown{reason:"quit"}` fires for Ctrl+C, Ctrl+D and `/quit`, and also for SIGHUP and SIGTERM [P extensions.md:393]. All exit 0 [P dist/modes/interactive/interactive-mode.js:3622-3649].

To tell them apart, prepend SIGHUP/SIGTERM listeners in `session_start`. Pi installs its own listeners with `prependListener` [P :3709-3724], so ours runs first. Mark ended only a quit with no signal, written synchronously [R pi/herdr-recover.ts:74-111]. SIGKILL, crash or power loss report nothing: the session stays resumable.

### 5. Resume

`cd CWD && pi FLAGS --session FILE`. Use the path, as legacy herdr does [H src/agent_resume.rs:126-129,234-236]:

- **By id.** An id that only exists in another project opens a y/N fork prompt, which hangs an unattended start. An unknown id exits 1 [P dist/main.js:237-261,352-370].
- **By path.** Pi reads the folder from the file header [P session-manager.js:1338-1359]. A missing path silently starts a new session [P :686-690], so check the file exists first. A missing file is a permanent failure.

### 6. Flags and env

| Item | Verdict |
|---|---|
| `--provider`, `--model`, `--models`, `--thinking`, `--session-dir`, `--tools`, `--exclude-tools`, `--no-tools`, `--no-builtin-tools`, `--no-skills`, `--no-context-files`, `--offline`, `--tui-mode` | restore |
| `-a/--approve`, `-na`: project-trust override. Pi has no per-tool approvals [P docs/security.md:3; cli.md:239-242] | restore (bypass class, opt-out applies) |
| `--no-extensions` | needs validation: it may disable herdr-wake's own extension |
| `-e`, `--skill`, `--prompt-template`, `--theme`, `--system-prompt`, `--append-system-prompt` | needs validation: these load code or instructions. [V core.js:69-76] never restores them; [T] does |
| `--session`, `--session-id`, `-c`, `-r`, `--fork`, `--no-session`, `-p`, `--mode`, `--api-key`, `--name`, prompts | never |
| `PI_CODING_AGENT_DIR`, `PI_CODING_AGENT_SESSION_DIR`, `PI_OFFLINE` | restore. Provider key variables: never |

### 7. Versions

The package was renamed from `@mariozechner/pi-coding-agent` at 0.74.0 (2026-05-07). Later milestones:

- 0.77.0: signals fire `session_shutdown`.
- 0.78.x: `ctx.mode` added.
- 0.79.4: signal-handler fix.
- 0.99.0: session file written late.
- 1.0.0 on 2026-10-01; 1.5.2 on 2026-11-10.

Sources: [P CHANGELOG.md:2456,2233,2161,2027,730,572]. Several releases a week. Minimum 0.79.4.

## OpenCode

| Line | Package | Releases |
|---|---|---|
| v1 | `opencode-ai` 1.x | latest 1.19.5 |
| v2 | `@opencode/cli` | 2.0.0 (2026-09-11), 2.0.28 (2026-10-13), latest 2.2.7 |

In v2, every TUI is a client of one shared background service [O2 cli/src/commands/handlers/default.ts:35-52].

### 1. Discovery: server events vs TUI polling

| | Server plugin (`event`, `parentID`) | TUI plugin polling the route |
|---|---|---|
| v1 | Runs in the TUI's worker [O1 cli/cmd/tui.ts:84]. Events are server-global, so an attached client can claim them [H opencode/herdr-agent-state.js:174-177] | `tui(api)`: `api.route.current.params.sessionID`, `api.state.session.get(id).parentID` [O1 plugin/src/tui.ts:594-634] |
| v2 | Runs in the shared service with whichever terminal's environment started it, so it cannot tell terminals apart [H :218-219; X plugins/opencode.js:6-7] | `setup(api)`: `api.ui.router.current()`, `api.data.session.root(id)`, `.get(id).location.directory` [O2 plugin/src/tui/context.ts:62-75,142-144,496-500] |

**Recommendation: use the TUI plugin.** Ship one module with `tui` (v1) and `setup` (v2) entry points and no `server` export, because v1 rejects a module that has both [O1 tui.ts:630-634]. There is no route-change event, so poll every 500 ms with `unref` [X :29-45; R opencode/tui.js:98-109].

```js
export default { id: "herdr-wake",
  setup(api) { if (!process.env.HC_TERMINAL_ID) return;
    const t = setInterval(() => { const r = api.ui.router.current();
      if (r?.type !== "session") return;
      const id = api.data.session.root(r.sessionID);
      report(id, api.data.session.get(id)?.location?.directory); // dedupe, spawn herdr-wake
    }, 500); t.unref?.(); return () => clearInterval(t); },
  async tui(api) { /* same loop: api.route.current, walk parentID */ } };
```

[R] reads `session.directory` [R opencode/tui.js:103]. In v2 the folder is `location.directory` [O2 cli/src/session-target.ts:51], so [R] falls back to `process.cwd()`.

**Install.** Let `CFG` be `$OPENCODE_CONFIG_DIR`, otherwise `${XDG_CONFIG_HOME:-~/.config}/opencode` [H env.rs:134-142]. Get the major version from `opencode --version`; if that fails, an existing `cli.json` means v2 [R install.sh:213-217].

- **v2.** Write `CFG/herdr-wake/tui.js` and add `"./herdr-wake"` to `plugins` in `cli.json`. JSONC edit, no duplicates. If `cli.json` is missing while `tui.json` or `kv.json` exists, wait until OpenCode has migrated [H src/integration/opencode_config.rs:53-74; R install.sh:219-227].
  - v2 also auto-discovers `CFG/plugins/<name>/tui.*` [O2 tui/src/plugin/discovery.ts:8-53; plugin/src/host.ts:25-58]. That needs validation on 2.0.x.
- **v1.** Write `CFG/herdr-wake-tui.js` and add it to `plugin` in `tui.jsonc` [H targets.rs:463-490].
- **Uninstall.** Remove the list entry and trash the files [H targets.rs:847-882].

### 2. Id changes

Switching, forking and the first prompt after `/new` move the route; the poll sees it. A child session maps to its root. Compaction keeps the id [H herdr-agent-state.js:198]. v2 TUI tabs [O2 context.ts `tabs`]: only the active tab gets recorded.

### 3. Skip

`run`, v1 `--mini`, `serve`, `web`, `acp` and `api` load no TUI plugin [H herdr-agent-state.js:121-133]. Resuming v1 `attach` / v2 `--server` sessions needs validation.

### 4. End

A quit exits 0. Signal exits drift: SIGHUP and SIGTERM exited 0 on 2.0.28 [R tui.js:13-16], but current source exits 143 on SIGTERM [O2 cli/src/index.ts:105-107,176]. Use the prepended signal listener plus `process.on("exit")` [R tui.js:74-85], not exit codes.

### 5. Resume

`cd CWD && opencode FLAGS --session ID` [H agent_resume.rs:249-255].

- **v1.** An unknown id, looked up in the current folder, exits 1 [O1 cli/tui/validate-session.ts:7-28; cmd/tui.ts:290-301].
- **v2.** The flag means "continue, or create if it does not exist" [O2 commands.ts:63-66]. If the lookup finds nothing, OpenCode silently opens a new chat [O2 handlers/default.ts:53-61,116-117].

**#5921** (open, no comments; OpenCode 2.2.6): after a cold reboot, panes open a new chat even though their command line holds the right `--session`. The log shows several `background service starting reason=missing` lines [I]. No root cause has been confirmed.

Likely mechanism: every TUI that finds no ready service starts competing service instances [O2 client/src/effect/service.ts:44-112]. Any "not found" answer during that window becomes a new chat with no error. Avoid:

1. Run `opencode service start`, then poll `opencode service status` until the service is ready.
2. Confirm the session exists with `opencode session list --format json`, run from its folder [O2 commands.ts:452-470]. That lists top-level sessions only, 100 by default. A missing session is a permanent failure.
3. Stagger the starts.
4. Mark the resume failed if the plugin has not reported the same id within about 15 s.

### 6. Flags and env

| Item | Verdict |
|---|---|
| `--auto`, and its hidden aliases `--yolo` and `--dangerously-skip-permissions` [O2 commands.ts:37-47; O1 tui.ts:147-161] | restore as `--auto` (bypass class) |
| v2 `--standalone`, `--server URL`; v1 `-m/--model`, `--agent` | restore. The v2 TUI has no model flag [O2 commands.ts:49-69] |
| v1 `--port`, `--hostname`, `--mini` | needs validation |
| `-s`, `-c`, `--fork`, `--prompt`, folder argument | never |
| `OPENCODE_CONFIG_DIR`, `XDG_*_HOME`, `OPENCODE_DB` [O2 database-path.ts:4-8] | restore |
| `OPENCODE_PASSWORD`, `OPENCODE_SERVER_PASSWORD` | never (the TUI strips them too [O2 env.ts:15-22]) |

### 7. Versions

v1 uses `tui.json` `plugin` with `tui(api)`; v2 uses `cli.json` `plugins` with `setup(api)`. herdr needed integration v13 to cover both [H CHANGELOG.md:110,172], and [R] dropped v1 [R README.md:217]. v2 ships about weekly, and its exit codes already drifted. `adopt` fallback: [M] reads `~/.local/share/opencode/opencode.db` [M agent_session.py:183-198].

## Adapter spec

### Pi

| Part | Spec |
|---|---|
| Install | `${PI_CODING_AGENT_DIR:-~/.pi/agent}/extensions/herdr-wake.ts` |
| Record | On `session_start` and `agent_start`, when `mode=="tui"` and `HC_TERMINAL_ID` is set: id, file, cwd, argv |
| Resumable | The session file exists |
| Ended | `quit` arrived with no SIGHUP/SIGTERM |
| Resume | `cd CWD && pi FLAGS --session FILE` |
| Permanent failure | Missing file or folder; an id lookup exits 1 |
| Minimum version | 0.79.4 |

### OpenCode

| Part | Spec |
|---|---|
| Install | v2: `CFG/herdr-wake/tui.js` plus a `cli.json` entry. v1: `CFG/herdr-wake-tui.js` plus a `tui.jsonc` entry |
| Record | Poll the route every 500 ms and take the root session id, folder and argv |
| Ended | Exit with no SIGHUP/SIGTERM seen |
| Before resume (v2) | Service ready, session exists |
| Resume | `cd CWD && opencode FLAGS --session ID` |
| Verify | The plugin reports the same id within 15 s |
| Minimum version | v1 floor unknown; v2 2.0.28 |
