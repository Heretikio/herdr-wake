# herdr-core interface for herdr-wake

Researched 2026-11-10 against herdr-core 0.1.20 (commit `de97ea5`, the version installed here, `hc --version` = 0.1.20). Paths are relative to the herdr-core repo root; `src/` means `crates/herdr-core/src/`. "Not found" means the core source does not answer it.

## 1. The `shell` config key

| Question | Answer | Source |
|---|---|---|
| File | `$XDG_CONFIG_HOME/herdr-core/config.toml`, default `~/.config/herdr-core/config.toml` (Windows `%APPDATA%`) | `src/paths.rs:24`, `src/platform/unix.rs:117-120`, `docs/security.md:60-65` |
| Type | `shell: Option<Vec<String>>`, TOML array: executable then separate arguments | `src/config.rs:11` |
| Validation | Must be non-empty with a non-empty first element, else a `config` error. No tilde or shell expansion. | `src/config.rs:50-56` |
| Default when unset | `$SHELL` (else `/bin/sh`), plus `-l` on macOS only | `src/platform/unix.rs:255-258` |
| New terminal argv | Config shell is used only when the caller gives **no argv and no line**. An explicit argv runs as given. | `src/api/run.rs:12-20` |
| Recovered argv | Always the config shell, replacing the stored argv. The original command is never re-run. | `src/kept.rs:55-61, 95` |
| Re-read live? | Yes. Read on every `terminal.run` and every recovery pass (once a second while anything is pending). No restart needed. | `src/api/run.rs:17,104`, `src/kept.rs:55`, `src/server.rs:71-77`, `README.md:73-79` |
| Program lookup | `Command::new(argv[0])` with a cleared, rebuilt environment; use an absolute path. | `src/platform/unix.rs:687-713` |

Child environment (both paths): the core's own environment (Linux: refreshed from the systemd user manager, `src/platform/mod.rs:310-332`), minus `TERM*`, `COLORTERM`, `HC_TERMINAL_ID`, `HC_SCOPE`, `HC_RUNTIME`; then `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=ghostty`, caller overrides, `HC_SESSION` if the core has one, `LANG` (`src/api/run.rs:228-251`). Then per terminal (`src/api/run.rs:212-225`):

| Variable | Value |
|---|---|
| `HC_TERMINAL_ID` | the address, e.g. `t1b` |
| `HC_TERMINAL_SOCKET` | the core's socket path |
| `HC_SCOPE` | random crash-cleanup token |
| `PWD` | the start folder |

No machine or core id variable is set (not found). Recovery also restores `restart_env` (the original caller's explicit `env` overrides, `src/kept.rs:82`, `src/api/run.rs:173`).

Working directory: new terminal uses the requested `cwd` (`src/api/run.rs:112`). Recovery uses the last observed foreground folder (`last_cwd`), else the start `cwd`, else `$HOME`, else `/` (`src/kept.rs:83-91`).

## 2. Self-identification, labels, and the protocol

A process learns its terminal from `HC_TERMINAL_ID`; `hc` trusts it only when `HC_TERMINAL_SOCKET` matches the socket it would use (`src/client.rs:285-289`). Always pass the id explicitly: `hc get` with no argument opens a picker in a TTY (`docs/commands.md:54-55`).

| Command | Syntax | JSON output |
|---|---|---|
| Read self | `hc get "$HC_TERMINAL_ID" --json` | full public `Terminal` record (`src/cli.rs:438-443`) |
| Write labels | `hc label "$HC_TERMINAL_ID" k=v k2=` (empty value removes) | full `Terminal` record (`src/cli.rs:461-465`, `src/terminals.rs:360-373`) |
| Find by label | `hc ls --label k=v [--all] --json` | `{terminals:[short rows], hidden_finished}`; short rows **omit labels, keep, created_ms** (`src/cli/output.rs:147-160`) |

Label keys: non-empty, no `=` (`src/terminals.rs:470-475`). Every `hc` call autostarts the core if absent (`src/cli.rs:230-233`). Public `Terminal` fields include `address, name, labels, argv, cwd, keep, state, pid, exit_code, signal, reason, created_ms, finished_ms, status, foreground, parent`; bookkeeping such as `program_started_ms`, `last_cwd`, `restart_env` is stripped (`src/terminals.rs:409-427`).

**Faster path (direct socket).** Transport: Unix socket at `$HC_TERMINAL_SOCKET` (Windows: named pipe). Plain mode is JSON-RPC 2.0, one message per line, requests handled one at a time per connection (`src/server.rs:168-228`, `schema/README.md`). A minimal client is: connect, write `{"jsonrpc":"2.0","id":1,"method":M,"params":P}\n`, read one line (`src/client.rs:267-281`). Errors carry `data.kind` and `data.hint`.

| Need | Method | Params |
|---|---|---|
| Version, features | `hello` | `{}` (reply has `version`, `core{id,name}`, `features`, `login_session`) |
| Core uptime | `server.status` | `{}` (`pid`, `uptime_ms`, `live`, `version`) |
| Get | `terminal.get` | `{terminal}` |
| Label | `terminal.label` | `{terminal, labels:{k:v}}` |
| List | `terminal.list` | `{all?, labels?:{k:v}}` (full records) |
| Events | `events.subscribe` | `{terminal?, labels?, children_of?}`; afterwards the plain connection carries only `event`, `heartbeat` (15 s), `missed` notifications |
| Opaque store | `doc.get` / `doc.set` | `{name}` / `{name, value, version}`, 1 MiB JSON, persisted in `docs.json` |

Framed mode (after `hello {framed:true}`) is only needed for attach. Full schema: `schema/hc.openrpc.json`; the API only grows (`schema/README.md`).

## 3. Persistence

State dir: `$XDG_STATE_HOME/herdr-core/<session>/`, default `~/.local/state/herdr-core/default/` on macOS and Linux, `%LOCALAPPDATA%\herdr-core\<session>\` on Windows (`src/paths.rs:17-23`, `docs/security.md:60-65`). Socket: `$XDG_RUNTIME_DIR/herdr-core/<session>.sock`, else `/run/user/<uid>/herdr-core/`, else the state dir (`src/platform/unix.rs:130-137`). On macOS, with no `XDG_RUNTIME_DIR`, that means the state dir.

| File | Content |
|---|---|
| `terminals.json` | `Registry {next_id, holder, terminals:{id: Terminal}, drafts}`; written atomically via `.tmp` + fsync + rename on a writer thread (`src/terminals.rs:208-217`, `src/runtime/persistence.rs`, `src/paths.rs:70-75`) |
| `core.json` | core id, name, machine id (`src/server.rs:116-159`) |

`keep` (`src/terminals.rs:21-23`) means: when the program is **lost** (`holder_lost`, `stopped_by_restart`, `server_stop`, `host_shutdown`, `signaled`), the record stays `live` and gets a new shell; an exit with a code or a kill is final (`src/terminals.rs:335-357`). Set by `hc run --keep`, `hc keep <t>`, or `terminal.run {keep:true}`. Whether the Herdr desktop or mobile app sets it by default: **not found** in core (the apps are not in this repo).

What survives: every field in the record, including `labels`, `name`, `created_ms`, `cwd`, `restart_env`. Core restart and update: the holder keeps programs running and the new core adopts them (`src/runtime.rs:295-345`); nothing is respawned. Reboot: live records no holder runs become `stopped_by_restart` (`src/runtime.rs:323-332`); kept ones get a shell. On recovery the core resets argv, cwd, template, title, status flags and screen (`src/kept.rs:94-106`) and on spawn clears `reason`, `exit_code`, `signal` (`src/runtime.rs:680-686`). **Labels are not touched.**

What wipes records:

| Trigger | Effect |
|---|---|
| Machine id changes (macOS `IOPlatformUUID`, Linux `/etc/machine-id`, Windows `MachineGuid`) | deletes `terminals.json`, `peers.json`, `screens`, `files`, `execs` (`src/server.rs:128-151`, `src/platform/unix.rs:567-580`) |
| Finished terminals | removed after 24 h or beyond 500 (`src/terminals.rs:9-10, 379-405`) |
| `hc rm`, deleting the state dir | gone |
| `hc server restart --fresh` | **does not** wipe records; it only replaces the environment for new terminals (`src/cli.rs:771-801`, `CHANGELOG.md:8`) |
| Corrupt `terminals.json` | core refuses to start, never silently resets (`src/paths.rs:59-68`) |

## 4. Recovery vs new, reboot vs restart

No field says "this launch is a recovery" once spawned: `reason` is cleared when the holder reports the pid (`src/runtime.rs:681-689`), which races the wrapper's first call. Workable signals:

- Labels already present on `$HC_TERMINAL_ID` (written by hooks in an earlier life) mean an old terminal. A brand-new terminal has only the caller's labels.
- `created_ms` older than this process's start by more than a few seconds.
- The `restarted` event (vs `created`) for subscribers (`src/runtime.rs:689`).

Reboot vs core restart: a core restart never relaunches the shell, so any wrapper launch in an old terminal means the holder was lost (reboot, logout on some setups, `hc server stop`, holder crash). To tell a reboot, compare the boot identity core itself uses (macOS `kern.bootsessionuuid`, Linux `/proc/sys/kernel/random/boot_id`, `src/platform/unix.rs:583-610`) with one stored in a label at record time. Core does not expose it over the API (not found).

## 5. Starting core

`hc server start` = `connect_or_start_explicit` then `server.status` (`src/cli.rs:551-574`, `src/client.rs:85-123`). It waits up to 15 s for the socket (`src/client.rs:223`).

| OS | Path |
|---|---|
| macOS | Unless `SSH_CONNECTION` is set, writes a one-shot plist to `/tmp/hc-launchd-*`, `launchctl bootstrap gui/<uid>`, `kickstart`, hands the environment and rlimits over a socket, then removes the job. Plist: `Label dev.herdr.core.<pid>.<ns>`, `ProgramArguments [exe, --hc-launchd]`, `ProcessType Interactive`, `RunAtLoad false`, `KeepAlive false`, `AbandonProcessGroup true`, stderr to `core.log` (`src/platform/macos_launchd.rs:148-255, 438-465`). If bootstrap fails (no GUI domain before login), it logs "starting directly (may end with the launching app)" and falls back to `setsid` (`:169-175`, `src/platform/unix.rs:204-219`). |
| Linux | `systemd-run --user --quiet --collect --service-type=exec` (simple below systemd 240), `-p OOMPolicy=continue`, `--unit=hc-serve-<hex8>`, `EnvironmentFile`, `RuntimeDirectory`, `KillMode=process`, `StandardError=append:core.log`, ordered `After=hc-shutdown-<hash>.service`; then best-effort `loginctl enable-linger` (`src/platform/systemd.rs:71-143, 244-326`). Needs `XDG_RUNTIME_DIR=/run/user/<uid>` and a running user manager, else plain `setsid`. The unit is transient: nothing restarts it at boot. |
| Windows | Detached `CreateProcess` (`src/platform/windows.rs:540-600`); an elevated SSH start is refused while a desktop session exists (`src/platform/windows_session.rs:13-27`). |

Races: `serve` takes `core.lock` non-blocking and a loser exits quietly (`src/server.rs:47-53`; clients: `src/client.rs:105-119`), so racing starts are safe, and so is a boot service with the right environment.

**Recovery is eager.** `serve` runs `recover_kept(true)` right after binding the socket, before any client, then every second (`src/server.rs:70-77`). It starts a holder itself (`src/kept.rs:51`). So agents can be warm before anyone connects, provided core starts.

macOS login trap: a core started outside a GUI login is **not** replaced when the user logs in; only a GUI core from an older login is (`src/platform/session.rs:11-14`, `src/platform/macos_session.rs:53-58`). Programs keep that pre-login audit session.

## 6. `hc connect` over SSH

`hc connect` pipes stdin/stdout to the socket via `connect_for_app`, which autostarts the core (`src/cli.rs:859-869`, `src/client.rs:77-79`). `hc machine add` dials `ssh … sh -c '… exec herdr-core connect'` with `BatchMode=yes` (`src/machines.rs:235-261`). So a desktop that added this machine starts its core on connect. Over SSH on macOS the launchd step is skipped and the core runs in the SSH login's session (`src/platform/macos_launchd.rs:148-156`). The mobile app's transport: **not found** in this repo.

## 7. OSC 7501

Program Status Protocol, parsed in vendored Ghostty. Keys today: `state` (idle, working, done, blocked, error, clear), `kind` (permission, question, auth), `progress` 0-100, `id` (sub-record), `app`, `title` and `msg` (base64); `?` queries supported states (`crates/ghostty-vt/src/lib.rs:283-297`, `src/status.rs:12-58`). Core stores it verbatim and never infers status (`AGENTS.md:34-36`, `CONTRIBUTING.md:5`).

Resume reference: herdr-core #15 (opened 2026-10-29, read on GitHub 2026-11-10) proposes `resume=<base64>` stored like `msg`. It is **open with no maintainer comment**; commenters prefer a full, live, clearable argv, and guillaume86 proposed `hc keep <t> --recover -- <argv>`. Nothing in 0.1.20 implements it.

## 8. Changelog hazards (0.1.10 to 0.1.20)

| Version | Change | Impact |
|---|---|---|
| 0.1.13 | Children end with their parent by default | agent sub-terminals die with the agent |
| 0.1.15 | Kept terminals come back after reboot (fixed); macOS shells become login shells | below 0.1.15 reboot recovery is broken |
| 0.1.16 | `shell = [...]` added | **minimum version** for herdr-wake |
| 0.1.19 | Restart keeps the core's original environment | wrapper env is frozen at core start |
| 0.1.20 | macOS core starts separately after login or reboot; `--fresh`; status cleared when an agent exits to a shell (only if the root process is named bash/zsh/fish..., `src/foreground.rs:143-155`) | wrapper must exec the real shell in place |

No registry format change is recorded; new fields use `serde(default)`.

## Implications for herdr-wake

**Can rely on:** `shell` is read live and used for every recovery; labels survive restart and reboot untouched; `HC_TERMINAL_ID` and `HC_TERMINAL_SOCKET` are in the child; recovery is eager at core start; plain JSON-RPC over the socket is a small, stable client.

**Risky:**
- `shell` applies to new terminals only when the client sends no argv. If the Herdr apps pass an explicit argv, only recoveries go through herdr-wake (not found).
- A shell that cannot start retries forever with backoff, never falling back (`README.md:76-79`, `src/kept.rs:1-26`). If the herdr-wake binary is missing, every terminal stays dead: uninstall must restore config first, and the config must use an absolute path.
- The wrapper must stay the parent of the agent and then exec the shell: if the agent becomes the terminal's program, a normal quit ends the terminal for good.
- Setting `shell` drops the macOS default `-l`; herdr-wake must add it.
- macOS pre-login: `launchctl bootstrap gui/<uid>` cannot work, the core falls back to a plain process in a non-GUI session and keeps it after login. Keychain-based agent credentials (Claude Code on macOS) may be unreadable. This challenges decision 10.
- `hc ls --json` hides labels; use `terminal.list` over the socket.
- Machine-id change wipes all records silently.

**Verify by live reboot test:** whether app-created terminals are `keep: true`; whether app new-terminal requests pass argv; labels present after reboot on both OSes; Linux core up before login with linger enabled by the installer; macOS pre-login core and Claude Code keychain auth; whether a phone connection starts core (`hc connect` path); recovery when two cores race at boot.
