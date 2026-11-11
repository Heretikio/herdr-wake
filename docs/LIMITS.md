# Limits and untested areas

Everything known that herdr-wake 0.1.0 does not do, does not handle, or has not been tested on. Written 2026-11-10. When a limit is removed, delete it here in the same change.

## What has not been tested

- **Real reboot tests cover Claude Code only.** Three were run on 2026-11-10: two on Linux x86_64 (the boot service started herdr-core before login) and one on macOS arm64 at the login screen (the first remote connection started herdr-core). Codex, Pi and OpenCode have never been run with herdr-wake as real agents. The phone app as first connection, the macOS LaunchAgent path (desktop login or auto-login) and `--before-login` have not been reboot-tested. See [TEST_RESULTS.md](TEST_RESULTS.md).
- **Pi and OpenCode plugins** have run only against fake hosts in Node, not inside real Pi or OpenCode.
- **Codex** hook handling, and binding a Codex session to its terminal while Codex runs its shared daemon, have been tested only against recorded fixtures.
- **Platforms.** The full suite has run on macOS arm64 (593 tests, 23 of them end-to-end against a real herdr-core 0.1.20) and on Linux arm64 in a container (595 tests, same end-to-end set). On Linux x86_64 only lint and the real reboot tests have run; the automated suite has not. Intel Macs: not tested. The end-to-end tests are unreliable under CPU emulation.
- **The security fixes** ([SECURITY_FIXES.md](SECURITY_FIXES.md)) are covered by automated tests only: no second user account, no real `sudo` run, no real LaunchDaemon install, no published release and no attestation have been tried. The Linux-only parts of them (the private systemd backup and linger marker, `CLOCK_BOOTTIME`) have only been compiled.
- **The end-to-end tests stand in for a reboot** by stopping and starting an isolated herdr-core. A real reboot also kills the holder process, changes the boot id, starts core from the boot job's environment with no desktop session or keychain, and lets the OS kill processes in its own order. None of that is covered.
- **Open questions from the prototype**, all still unanswered:
  - Do terminals created by the Herdr desktop and mobile apps have herdr-core's `keep` flag set? herdr-core only recovers kept terminals. If the apps create unkept terminals, nothing resumes in them.
  - Does the phone app's connection start herdr-core?
  - On a Mac before login, does Claude Code come up signed in?
  - Does `/exit` in Claude Code report `prompt_input_exit` to its `SessionEnd` hook?
  - Does `hc server restart` on a core started before login keep its terminals?
- **WSL** is untested. The boot service needs `[boot] systemd=true`, nothing starts WSL when Windows boots, and an idle distro stops after about 15 seconds. `doctor` warns.
- **Windows** is not built yet ("coming soon"). The installer stops with a message.

## Flags and permissions

- **Bypass flags are restored by default.** After a reboot, agents start with nobody watching, with `--dangerously-skip-permissions`, `--yolo` and similar flags if they had them. `restore_bypass = false` turns that off.
- **`restore_bypass = false` drops the launch flags that loosen permissions, and only launch flags.** These are dropped:
  - Claude Code: `--dangerously-skip-permissions`, `--allow-dangerously-skip-permissions`, `--permission-mode` with any mode other than `default`, `manual`, `plan` or `dontAsk`, `--settings`, `--allowed-tools` / `--allowedTools`, `--tools`.
  - Codex: `--dangerously-bypass-approvals-and-sandbox`, `--yolo`, `--approve-for-me`, `--not-so-yolo`; `-s` / `--sandbox` unless the value is `read-only` or `workspace-write`; `-a` / `--ask-for-approval` unless the value is `untrusted`, `on-request` or `on-failure`; every `-c` / `--config` whose key, or any dotted part of it, is `approval_policy`, `sandbox_mode`, `profile` or starts with `sandbox_`; `-p` / `--profile`.
  - Pi: `--approve` / `-a`.
  - OpenCode: `--auto` and its aliases `--yolo` and `--dangerously-skip-permissions`.

  Not covered: anything an agent's own config files allow (Claude's `settings.json` at user or project level, Codex's `config.toml`, a profile selected there), Claude `--add-dir`, `--mcp-config`, `--plugin-dir` and `--agents` (still restored: they add reach or code, not a permission mode), and Codex `-c` keys other than the ones listed (for example `shell_environment_policy`). An unknown value for `--sandbox` or `--ask-for-approval` counts as loosening and is dropped. Dropping `--settings` or `--profile` also drops whatever harmless settings they carried.
- **With `restore_bypass = false` at record time, bypass flags are not stored.** Switching it back to `true` later does not bring them back for sessions recorded meanwhile.
- **Flags are filtered through fixed tables**, built from Claude Code 2.1.322, Codex source from November 2026, Pi 1.5.2 and OpenCode 2.2.7. A flag not in the table (for example one added in a later agent release) is left out of the resume command. Flags marked "needs validation" are left out too: for Claude `--remote-control`, `--bare`, `--safe-mode`, `--autocompact` and a few others; for Pi `-e`, `--no-extensions`, `--skill`, `--prompt-template`, `--theme`. The plan to re-check saved flags against each agent's current `--help` is not built.
- **Some flag values are never stored**, because they often hold secrets: inline JSON for Claude `--settings`, `--mcp-config` and `--agents` (the flag is dropped whole; a file path is kept), a Codex `-c` override whose key or value contains `key`, `token`, `secret`, `password`, `auth` or `bearer` in any case (this also drops harmless ones such as `preferred_auth_method`), and any flag value with a URL that has a user name or password in it (OpenCode `--server` keeps the address without them). The session is still recorded and resumes without that flag; the log names the flag. This is pattern matching: a secret in any other flag value (a system prompt, a model name field) is stored as typed, in a label every client of your herdr-core can read. Do not put secrets on an agent's command line.
- **Provider settings in the environment are not restored.** Only `PATH` (its absolute folders), `LANG`, `LC_*`, `XDG_*_HOME` and the agents' own folder variables are kept; `OPENCODE_DB` only when it is an absolute file path. A Claude Code session that relied on `ANTHROPIC_BASE_URL`, `CLAUDE_CODE_USE_BEDROCK` or `CLAUDE_CODE_USE_VERTEX` in its shell gets those only if herdr-core's own environment has them.

## Quit or kill: telling them apart

herdr-wake resumes only agents that were running when the machine went down. Telling "the user quit" from "the machine killed it" is not always possible.

- **An agent you started by hand and then killed, while its shell keeps running, counts as a quit.** A detached watcher sees the agent gone and the shell alive, and marks the record ended. This includes `kill` from another terminal, the OOM killer and a crash. That session will not be resumed.
- **An agent herdr-wake resumed is judged by its exit status instead.** Exit 0, exit 130 or SIGINT is a quit. SIGHUP, SIGTERM or SIGKILL keeps the record. So the same `kill` gives a different result for a resumed agent than for a hand-started one.
- **Pi, and OpenCode 2.0.28, exit 0 when they receive SIGHUP or SIGTERM.** For a session herdr-wake resumed, an exit 0 counts as a quit. If such an agent is signalled while herdr-wake's own process survives, the record is ended. Not yet seen with the real agents.
- **Shutdown detection on macOS** listens for the login window's restart and shutdown notifications. It does not see shutdowns that bypass the login window (`shutdown -r now`, `reboot`, power loss, kernel panic), it does not see notifications posted before the watcher started, and the notification API can report false positives. During a shutdown it does not detect, an interactive zsh (which ignores SIGTERM) can outlive the agent and make the kill look like a quit. Every other doubt (the watcher itself being killed, a new boot id, an unreachable core) keeps the record.
- **Shutdown detection on Linux** asks logind (`PreparingForShutdown`), `systemctl is-system-running` and systemd's scheduled-shutdown file. A hard reset or power loss posts nothing, but also kills everything at once, so nothing gets marked ended.
- **Codex sends no end event on `/quit`.** Its `SessionEnd` fires only on archive, delete or 30 minutes idle, always with reason `other`. A Codex quit is known only from the watcher or the exit status.
- **Claude Code's `SessionEnd` reason `other`** is ambiguous and is treated as "keep". Hooks during a shutdown may not finish.

## herdr-core

- **herdr-core ships a release every one to three days and has no stable API for this.** herdr-wake uses its socket protocol, terminal labels and the `shell` setting. A core update can break herdr-wake without warning. A nightly job is set up to run the test suite against the newest core; it has not run yet. Run `herdr-wake doctor` after updating core.
- **Minimum herdr-core 0.1.16.** `doctor` checks it.
- **A broken `shell` setting is invisible.** If the herdr-wake binary is missing or not executable, herdr-core retries forever, shows the terminal as live with no process, and writes the error only to `holder.log`. `doctor` detects it by watching for kept terminals with no program for 5 seconds. A fixed setting takes up to 60 seconds to apply. See the README, "Before you install", point 2.
- **Only kept terminals come back.** herdr-core does not recover terminals without `keep`, so nothing resumes in them.
- **Every recovered terminal starts `herdr-wake shell`**, including one that was created with its own command (for example `hc run --keep -- htop`). After a recovery that terminal shows your normal shell, not the command. This is herdr-core's behavior with any `shell` setting.
- **A core started before login on macOS** (with `--before-login`) lives outside the desktop session: no clipboard, no `open`, no keychain prompts in its terminals until someone runs `hc server restart`. Whether that restart keeps the terminals is untested.
- **Two cores.** If the boot job's `HOME`, `XDG_STATE_HOME` or `HC_SESSION` differ from your shells', a later client starts a second core instead of finding the first.
- **`boot.env` is a snapshot.** It is taken from the shell you install from. When your `PATH` changes (a new Node version manager release, a new tool folder), run `herdr-wake install` again. `doctor` warns when a recorded agent's folder is missing from it.
- **Boot timing.** `herdr-wake boot` waits up to 30 seconds for a network address, then starts core anyway, then waits up to 120 seconds for the resumes to settle. Anything still waiting then is counted as failed in the boot summary.

## Records

- **A record is resumed only when it matches herdr-wake's local copy** (`~/.local/state/herdr-wake/records/<terminal>.json`, written by the hook, `adopt` and `resume <terminal>`). Consequences:
  - Records from a build that kept no copies show as `untrusted` and are not resumed until the agent starts again, `herdr-wake adopt` (Claude Code) or `herdr-wake resume <terminal>`.
  - Deleting or moving the state folder, or restoring it from a backup of another moment, makes every record untrusted.
  - The state folder itself must not be a symlink (herdr-wake does not write through one), so a dotfile manager that links `~/.local/state/herdr-wake` elsewhere stops records from being trusted.
  - `herdr-wake resume <terminal>` accepts whatever the label says at that moment. Read the command it prints.
  - The copy is compared on agent, session id, session file, folder, flags, environment and program, plus the terminal's creation time. The state is not part of it: the copy of a session you quit is deleted instead, so its label cannot be flipped back to running.
- **What a record may run is checked at wake**, and a record that fails is given up on with one line. The checks can refuse a legitimate setup:
  - The program must be owned by you or root and not writable by group or others. A Homebrew on Linux installed under the `linuxbrew` user, or a group-writable tool folder, fails.
  - The program's file name (as recorded, or after resolving symlinks) must be the agent's (`claude`, `codex`, `pi`, `opencode`, also with `.js`), or for Claude a file under `claude/versions/` or `claude-code/cli.js`. An agent started through a wrapper script under another name is recorded but not resumed.
  - A record whose program is a bare name (the agent could not be found on `PATH` when it was recorded) is not resumed.
  - Pi's session file must be a regular file you own inside Pi's session folder (`PI_CODING_AGENT_SESSION_DIR`, else `${PI_CODING_AGENT_DIR:-~/.pi/agent}/sessions`, or the folder of a recorded `--session-dir`).
  - A session id with characters outside letters, digits, `.`, `_` and `-` is neither recorded nor resumed.
- **Folder safety reads modes and owners only.** Access control lists are not read, so a folder that an ACL opens to others passes, and group-writable folders are accepted only when the group is your own private group (your primary group, named like you, with no other member), which is what Fedora, Debian and Ubuntu create together with umask 002. A folder writable by any other group, such as `staff` on macOS, is refused until `chmod g-w`. A folder above the session's that is owned by another user is refused even when it is not writable by anyone else; root-owned and own group-writable parents (`/opt/homebrew`, `/Users/Shared` with its sticky bit) pass. The check runs when a session is recorded and when it is resumed; a folder that changes hands in between is caught only at the next wake.

- **Label writes are not atomic.** herdr-core has no conditional write. Every writer re-reads the record and checks its sequence number and agent process first, then writes; two writers that land in the gap between that read and that write (for example an agent's hook and herdr-wake's own process at the same instant) can lose one update.
- **Records live on the terminal.** Closing a terminal deletes its record. A record written by another herdr-wake schema version is ignored, and an older build never overwrites a newer record.
- **Attempts count per boot.** A session that used its 3 attempts is tried again after the next boot. A record marked failed (a permanent failure, or attempts used up) stays failed until `herdr-wake resume <terminal>`.
- **Clocks.** Gate slots, the stagger and the boot run's duration use a monotonic clock (`CLOCK_BOOTTIME` on Linux, `CLOCK_MONOTONIC` on macOS), so a wall clock that is stepped during boot does not disturb them. Record sequence numbers and log timestamps still use the wall clock: after a backwards step, log lines can appear out of order, and the boot summary's `started_ms` shows the time as the clock read then.
- **Starts are staggered per agent kind.** The first agent of each kind starts alone for up to 8 seconds (less once it reports in), the rest 1.5 seconds apart, in whatever order their terminals reach the gate. With many sessions the last ones start noticeably later. The spacing is meant to stop concurrent Claude Code processes from racing for one login token (Claude Code issue 76905); whether 1.5 seconds is enough is unverified.

## Claude Code

- **Keychain before login (macOS).** Claude Code keeps its login in the macOS login keychain, which stays locked until someone logs in at the screen. With `--before-login`, a resumed Claude session may come up signed out. Workarounds: `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token`, or Claude's credentials file.
- **Hooks can be switched off.** `disableAllHooks` or `allowManagedHooksOnly` (in your settings or a managed settings file) stop herdr-wake's hooks. `--bare` and `--safe-mode` runs skip hooks. `doctor` checks the settings.
- **`adopt` reads an undocumented file**, `<config dir>/sessions/<pid>.json`. A Claude Code release can change it.
- **A missing session** is detected by looking for its transcript under the recorded config folder. Claude Code's own exit code for an unknown id has not been checked.

## Codex

- **Approve the hook once.** Codex runs a new user hook only after you approve it in `/hooks`. Until then, Codex sessions are not recorded. herdr-wake cannot read that approval, so `doctor` always shows a reminder.
- **Two sessions in one folder.** With Codex's shared daemon, the hook cannot say which terminal it came from. herdr-wake matches the session to the terminal whose foreground program is Codex and whose argument list names the session id, or else whose folder matches. If two terminals run Codex in the same folder and neither names the id (two fresh starts), the match is ambiguous and the session is not recorded. There is no start-time tiebreak (it would need Codex's SQLite database, which herdr-wake does not read).
- **Timing.** herdr-core refreshes a terminal's foreground program about twice a second. The hook retries for about 2 seconds; a slower refresh leaves the session unrecorded.
- **`[features] hooks = false`** in Codex's config switches hooks off. `doctor` checks.
- **Sub-agent filtering** reads the first line of the session's rollout file. Compressed rollouts (`.jsonl.zst`) and rollouts not written yet skip that check. Codex 0.166 leaked its auto-review thread into the pane (Herdr issue 5837); versions between 0.166 and the November 2026 source were not checked.
- **No fallback to `--no-daemon`.** The planned fallback (turning off Codex's daemon with your consent) is not built.
- **Keyring credentials** (`cli_auth_credentials_store = keyring`) before login have the same open question as Claude Code's keychain.

## Pi

- **Minimum 0.79.4.** `doctor` warns about an older Pi when `pi --version` answers within 3 seconds and prints a version; otherwise it says nothing about the version.
- **A session with no message has no file** (Pi writes it at the first prompt), so there is nothing to resume. `--no-session` runs are not recorded.
- See also "Pi exits 0 on SIGHUP or SIGTERM" under [Quit or kill](#quit-or-kill-telling-them-apart).

## OpenCode

- **v2 opens a new chat for a missing session.** OpenCode v2 treats an unknown session id as "create it". herdr-wake cannot check beforehand. After the resume it waits 15 seconds for the plugin to report the same session id; if it does not, the log gets an "unconfirmed" line and the boot summary counts the terminal as unconfirmed. The new chat is left running.
- **Background service.** Before a v2 resume herdr-wake runs `opencode service start` (up to 30 seconds), to avoid the competing services behind Herdr issue 5921. Not yet tried with real OpenCode.
- **v1 or v2 at install is decided by `opencode --version`** (2 or newer is v2). Only when the version cannot be read (OpenCode's folder exists but `opencode` is not on the `PATH` you install from) does an existing `cli.json` decide; then installing before OpenCode v2 has run once sets up the v1 plugin. At resume the old rule still applies: a `cli.json` in OpenCode's folder means v2 (the `service start` step and the 15 second confirmation).
- **Only the active tab** of a v2 TUI is recorded. Sessions opened with v1 `attach` or v2 `--server` were not checked.
- **v1 minimum version is unknown.** `doctor` warns about an OpenCode 2.x older than 2.0.28 when `opencode --version` can be read, and has no floor for version 1.
- **The plugin polls** the TUI's route every 500 ms, because OpenCode has no route-change event.

## macOS

- **Default mode starts at login.** On a Mac without auto-login, agents come back when you log in at the screen, or when the first client starts herdr-core (opening Herdr, the phone app, or `hc` over SSH).
- **FileVault** makes auto-login unavailable. A `--before-login` daemon waits until the disk is unlocked (on macOS 26 or newer on Apple silicon, an SSH unlock can do that, or `fdesetup authrestart` before a planned restart). herdr-wake does not work around FileVault in 0.1.0.
- **Protected folders.** A core started before login cannot show the permission prompts for Documents, Desktop or Downloads, so agents working there may be refused access.
- **Login items.** macOS lists the boot job under Login Items and may show a notification when it is added.

## Linux

- **systemd is required**, as PID 1 with a user manager. Without it (OpenRC, cron-only systems), nothing starts herdr-core at boot. `doctor` fails the `init` check. Running `herdr-wake boot` from a `@reboot` cron entry may work but is untested.
- **Linger** makes the user service start at boot. Install turns it on with `loginctl enable-linger`; some distributions require admin rights for that, and then the service starts at your first login. `doctor` checks.
- **User services see a reduced environment.** `~/.local/bin`, nvm, bun and cargo folders are usually missing; `boot.env` fills them in from install time. Without a desktop login there is no session D-Bus.

## Install and uninstall

- **The recorded binary path must stay put.** Install writes the path it was run through (symlinks not resolved) into herdr-core's `shell` setting and every agent hook. If that file later disappears, terminals cannot start and hooks fail (see the README, "Before you install"). A path that names one version (under `/Cellar/`, under `/versions/`, or through a folder named like a version number) is never recorded: install uses a symlink to the same file from `PATH`, `/opt/homebrew/bin`, `/usr/local/bin`, `/home/linuxbrew/.linuxbrew/bin` or `~/.local/bin`, and refuses when it finds none. The rule is a pattern: a versioned layout it does not recognise (a Nix store path, for example) is recorded as it is. There is no Homebrew formula yet, and this has not been tried with a real Homebrew install.
- **Install sets up only agents it finds**: a config folder that exists, or the agent's program on the `PATH` of the shell you install from. An agent installed later, or one that is on `PATH` only in another shell, is skipped until you run `herdr-wake install` again. Uninstall reverts every agent and removes folders left empty, up to three levels; an empty folder you made yourself before install (an empty `~/.pi/agent`) goes with them.
- **`--before-login` on macOS needs `sudo` steps you run yourself**, printed by install and by uninstall. There is no root mode: `sudo herdr-wake install` is refused. Between staging and your `sudo install`, the staged plist sits in a folder only you can read or write; a process running as you could still change it, so read it (or the `plutil -lint` output) before you install it. The daemon has no launchd log file: look in `~/.local/state/herdr-wake/herdr-wake.log`.
- **Never as root.** `install`, `uninstall`, `adopt`, `resume`, `forget`, `off` and `on` refuse to run with an effective uid of 0. `status` and `doctor` do not refuse, but run as root they read root's home, not yours.
- **`--host`** needs SSH access, copies to `~/.local/bin` on the other machine, and refuses when that machine's OS or CPU differs from this binary's.
- **The installer checks a SHA-256 from the same release.** That catches a damaged download, not a tampered release. The release workflow is set up to attach a build provenance attestation (`gh attestation verify`), but the installer does not check it, and no release has been built yet, so the attestation step has never run. A download cut off inside the script's last line could still run the installer without its arguments (audit L-9, not fixed).
- **Backups are private but complete.** Each file is copied once per run, mode 0600, and the copies hold whatever the file held (API keys in an agent's settings). They are never pruned and survive uninstall.
- **Uninstall leaves** logs and backups in `~/.local/state/herdr-wake/`, and the record labels on terminals.

## Settings

- **herdr-wake never types into a resumed agent.** It starts the agent and waits; it sends no "continue". (`auto_continue` was removed from the settings: it never did anything. A config file that still has the key loads fine; the key is ignored.)
- **`exclude_dirs` takes absolute paths or paths starting with `~/`.** Relative entries and `~name` never match. The same rule applies when a session is recorded, when a terminal wakes and in the boot summary. Paths are compared as written: a folder reached through a symlink is not matched by its real path.
- **A `config.toml` that cannot be parsed stops everything until it is fixed.** Nothing is resumed (plain shells, one log line per terminal), nothing is recorded, and `status`, `forget`, `resume`, `adopt`, `off`, `on`, `install` and `uninstall` stop with the parse error. `doctor` runs and reports it as a failed check. herdr-wake does not fall back to the defaults, which include `restore_bypass = true`.
- **`herdr-wake off <terminal>` lasts as long as the terminal.** It is a label on that terminal (`wake.off`), so it goes when the terminal is closed, and anything that can write labels can remove it. An agent already running in the terminal when you switch it back on is not recorded until it starts again. `herdr-wake forget <terminal>` removes the current record only.

## Not in 0.1.0

- A notification after boot. The summary ("resumed", "failed", "skipped", "unconfirmed") is shown by `herdr-wake status` only.
- Re-checking saved flags against each agent's current `--help` (decision 15). The flag tables are compiled in.
- Sending anything to a resumed agent ("continue"). `auto_continue` is not a setting.
- Snapshots, named spaces, layout restore (herdr-core restores layout itself), resuming sub-agents.
- Agents other than Claude Code, Codex, Pi and OpenCode.
- The Herdr TUI, which resumes agents by itself.

## Found in real reboot testing (2026-11-10)

- **Claude Code's folder trust prompt blocks an unattended resume.** If Claude Code has not been told to trust the session's folder, the resumed agent stops at "Is this a project you created or one you trust?" and waits for a person. It asks every time for the home folder. herdr-wake starts the agent, cannot answer the prompt, and reports the terminal as `unconfirmed` in the boot summary. Start agents in project folders you have already trusted, not in your home folder.
- **Mode changes made inside the agent are not restored.** herdr-wake records the flags an agent was started with. If you switch Claude Code to auto mode or plan mode with shift+tab during the session, the resumed agent starts in the mode its launch flags give it.
- **A hardware clock that is wrong at boot.** On the test machine the system clock was stepped back 130 s during every boot. Timing inside herdr-wake no longer depends on the wall clock, but timestamps in `herdr-wake.log` and `last-boot.json` from the first seconds of a boot can be out of order.
- **herdr-core shows the terminal's agent as "herdr-wake"** in `hc ls` until an agent's own status reaches core, because the terminal's program is the wrapper.
- **A terminal created with `hc run` does not appear in the Herdr desktop app.** The desktop app shows terminals that carry its own space and tab labels. herdr-wake resumes command-line terminals all the same, and the phone app lists them.
