# herdr-wake: module design

Built on `DECISIONS.md` and the four docs in `research/`. Vocabulary follows the codebase-design skill: module, interface, seam, adapter, depth.

## The three flows

**Record** (while the machine is up). An agent's hook or plugin runs `herdr-wake hook <agent> <event>` with a JSON payload on stdin. herdr-wake decides whether the session counts (not a sub-agent, not a one-shot run), works out which herdr-core terminal it belongs to, and stores a record on that terminal as a label. If the agent was started by the user, it also starts a small detached watcher (`herdr-wake watch`) that waits for the agent to exit.

**Forget** (while the machine is up). When an agent exits and its shell is still alive a moment later, the user quit: the record is marked ended. If the shell died with it, the machine is going down: the record stays. A watcher that is itself killed at shutdown leaves the record in place, so every failure of this check leans toward resuming.

**Wake** (after a reboot). The boot service runs `herdr-wake boot`, which starts herdr-core. Core recreates each kept terminal at once and launches its configured `shell`, which is `herdr-wake shell`. That process reads the terminal's record. No live record: it replaces itself with the user's real shell. Live record: it waits its turn at the gate, starts the agent as a child, and when the agent exits it replaces itself with the real shell.

## Modules

| Module | Interface (what callers must know) | Hides |
|---|---|---|
| `core` | `Core` trait: `hello`, `get(terminal)`, `label(terminal, changes)`, `list(filter)`. `Core::connect()` never starts a server. | JSON-RPC over the Unix socket, socket path rules, timeouts, version and feature checks. Adapters: `SocketCore`, `FakeCore`. |
| `record` | `Record` (agent, session id, session file, cwd, launch flags, allowlisted env, binary path, agent pid and start time, shell pid and start time, boot id, state, attempts, sequence). `Store::load/save/end/forget(terminal)`. | The label key and JSON encoding, schema version and migration, ordering by sequence so a late event cannot overwrite a newer one, the claim lock that stops two terminals owning one session. |
| `agents` | `Agent` trait, one adapter per agent: `observe(event, payload, facts) -> Option<Observation>`, `resume_plan(record, probe) -> Plan or Permanent(reason)`, `integration() -> files and edits to install`, `confirm(record) -> bool`. | Everything agent-specific: hook payload shapes, skip rules, Codex terminal binding through the pane's foreground process and state database, flag policy tables, Pi's session-file rule, OpenCode's service warm-up and v1/v2 split. Adapters: `claude`, `codex`, `pi`, `opencode`. This is the swappable session-id source from decision 11. |
| `platform` | `Platform` trait: `process(pid) -> argv, env, start time, parent`, `alive(pid, start)`, `boot_id()`, `boot_service() -> install, remove, state`, `exec(argv) -> !`, `login_shell()`. | `KERN_PROCARGS2` versus `/proc`, launchd versus systemd, FileVault and auto-login probes, linger. Adapters: `macos`, `linux`, `fake`. Windows is a third adapter later. |
| `wake` | `decide(record, facts, config) -> Decision` (pure), and `run(decision)` which performs it. | The resume rule, opt-outs, up to 3 attempts, permanent versus transient failures, the manual command message, waiting on the child, quit versus kill from the exit status. |
| `gate` | `Gate::enter(agent) -> Permit`. | Stagger across independent `herdr-wake shell` processes: a file lock plus timestamps. First agent of each kind starts alone and is confirmed, the rest follow 1.5 s apart. |
| `setup` | `install(options) -> Report`, `uninstall() -> Report`, both with dry-run. | Order of operations, backups, comment-preserving edits to JSON, JSONC and TOML, the saved real shell, remote install over SSH. |
| `report` | `status()`, `doctor()`, `boot_summary()`. | Read-only checks. Never calls `hc` (any `hc` command starts core). |
| `cli` | Subcommands: `install`, `uninstall`, `status`, `doctor`, `forget`, `resume`, `adopt`, plus internal `hook`, `watch`, `shell`, `boot`. | Argument parsing only. |

Two seams are real because they have several adapters each: `Agent` (four) and `Platform` (two now, three later). `Core` has a production adapter and a fake for tests. Everything else is a plain module with no trait.

## Rules the code must never break

1. **`herdr-wake shell` cannot fail.** Any error, timeout (2 s budget for core calls) or panic ends in the real shell. If the saved real shell is missing, use `$SHELL`, then `/bin/sh`. Core retries a shell that cannot start forever and never falls back, so this process is the only safety net.
2. **Uninstall restores core's `shell` setting first and removes the binary last.** The config always holds an absolute path. Upgrades replace the binary by atomic rename. `doctor` checks that the configured path exists.
3. **The wrapper stays the parent of the agent, then replaces itself with the real shell.** If the agent became the terminal's own program, quitting it would end the terminal for good.
4. **Add the login flag core drops.** On macOS, setting `shell` removes the `-l` core adds by default; the real shell is started as a login shell there.
5. **Store facts, rebuild the command.** A record holds id, folder, flags and env. The resume command is built at wake time from the agent's current rules and flags are re-checked against its `--help`. A cached command line is how legacy Herdr resumes the wrong session after `/clear`.
6. **No secrets in records.** Env is an allowlist (`PATH`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_DIR`, `OPENCODE_CONFIG_DIR` and similar). Anything matching `*_KEY`, `*_TOKEN`, `*_PASSWORD` is never stored.
7. **Never check on core with `hc`.** Connect to the socket directly, or look for the process.
8. **Edits to other tools' files are surgical.** Add or remove only herdr-wake's own entries, keep comments and order, back up first. Codex hook trust is hash-bound, so the hook command string never changes between versions.

## Corrections from the prototype

The prototype (`research/prototype-findings.md`, run 2026-11-10 against an isolated herdr-core 0.1.20) confirmed the mechanism and forced these changes. Where this section and the text above differ, this section wins.

1. **Rule 3, sharpened.** The wrapper runs the agent in its own process group as the terminal's foreground job, takes the terminal back when the agent exits, restores every signal disposition it changed (ignored signals survive exec), then replaces itself with the real shell. Without this, core reports the wrapper as the foreground process, which breaks Codex binding and core's status clearing.
2. **Rule 1, sharpened.** The 2 s budget is one deadline across connect, write and read. A hung core still accepts connections.
3. **Forget flow.** "The shell outlived the agent" is not safe during shutdown: interactive zsh ignores SIGTERM, so a kill can look like a quit. Order of evidence for a deliberate quit: the agent's own end event, then the wrapper's exit status when it launched the agent, then the watcher. The watcher must exit on SIGTERM without writing, and it checks for a shutdown in progress before it marks anything ended (`Platform::shutting_down()`: loginwindow shutdown and restart notifications on macOS, logind `PrepareForShutdown` on Linux). This check goes on the reboot matrix.
4. **Record writes are compare-and-set.** Every writer, the wrapper included, re-reads the record and checks `agent_proc` and `seq` before writing. The prototype's first wrapper overwrote a newer record with its stale copy.
5. **Gate.** The gate file is keyed by boot id plus core pid, so a file from a previous boot is ignored. "Confirmed" means the resumed agent's own hook re-stamped the record with the child's pid; keep the timeout as the fallback.
6. **Every kept terminal becomes a wrapper terminal after its first recovery**, including ones created with an explicit command. A record from an agent that was the terminal's own program (for example `hc run claude`) has core's holder as its shell; treat that as "no shell to outlive".
7. **A broken `shell` setting is invisible in core's interface.** `doctor` looks for kept, live terminals with no process for more than a few seconds and reads core's `holder.log`. A repaired config can take up to 60 s to apply; uninstall and repair output say so.
8. **`boot.env` also carries `SHELL`, `USER`, `LOGNAME`, `TMPDIR`.** Core's terminals had none of them in the prototype.
9. **Test harness.** The throwaway-home core must set `socket = "/private/tmp/hw-<n>/c.sock"` in its config (the default path exceeds the 103-byte limit) and set `SSH_CONNECTION`, or core's macOS start path touches launchd jobs shared with the developer's real core.

Measured: recovery starts 0.19 s after core starts and before any client; a socket round trip takes 0.05 ms; staggered starts landed 1.49 to 1.56 s apart.

## Where state lives

| What | Where |
|---|---|
| Session records | Labels on each herdr-core terminal. They survive restart and reboot and disappear with the terminal. |
| Settings | `~/.config/herdr-wake/config.toml`: on/off, restore bypass (default on), auto-continue (default off), attempts, stagger, excluded folders, the saved real shell. |
| Boot environment | `~/.config/herdr-wake/boot.env`, mode 0600: `PATH`, `LANG`, `LC_*` captured at install. |
| Log, gate file, last boot summary, backups | `~/.local/state/herdr-wake/` |
| Boot service | macOS: `~/Library/LaunchAgents/dev.heretik.herdr-wake.plist` (default) or a LaunchDaemon with `--before-login`. Linux: `~/.config/systemd/user/herdr-wake.service` plus linger. |

## Testing

- **Unit tests** drive `wake::decide`, `record`, each `Agent` adapter and `setup` edits through their interfaces with `FakeCore` and the fake platform. Agent adapters are tested against payload fixtures captured from the real tools.
- **Integration tests** run a real herdr-core in a throwaway home directory, so it has its own state, config and socket and cannot touch the developer's sessions. Stopping and starting that core reproduces what a reboot does to kept terminals. Stub agents (small scripts that fire the same hooks) stand in for the real ones.
- **The reboot matrix** from decision 13 runs on real hardware with real agents and is the only thing that earns a "supported" label.
- **Nightly**: the integration suite against the newest herdr-core release.

## Minimum versions

herdr-core 0.1.16 (`shell` setting; reboot recovery fixed in 0.1.15). Pi 0.79.4. OpenCode 2.0.28 for v2. `doctor` reports anything older.

## Open points for the prototype

1. Do terminals created by the Herdr desktop and mobile apps have `keep` set? Unkept terminals are not recovered, so nothing would resume in them.
2. Does the phone app's connection start core?
3. On a Mac before login, does Claude Code come up signed in?
4. Does `/exit` in Claude Code report `prompt_input_exit`?
5. Does `hc server restart` on a pre-login core keep its terminals?
