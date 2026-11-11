# herdr-wake

herdr-wake brings your coding-agent sessions back after a reboot when you run them in [Herdr](https://herdr.dev) (the desktop or mobile app, which both use herdr-core). It starts herdr-core when the machine boots, and in each terminal that had an agent running it resumes that same session, in the same folder, with the same flags.

```sh
curl -fsSL https://github.com/Heretikio/herdr-wake/releases/latest/download/install.sh | sh
```

Then run `herdr-wake doctor`. Prefer to let your coding agent do it? Paste [docs/INSTALL_PROMPT.md](docs/INSTALL_PROMPT.md) into it.

> **Status (2026-11-10): real reboot tests have been run for Claude Code only.** On a Fedora 44 x86_64 machine, the boot service started herdr-core before anyone logged in and herdr-wake resumed a real Claude Code session. On an Apple silicon Mac sitting at its login screen, herdr-wake resumed two real Claude Code sessions within four seconds of the first connection. Codex, Pi and OpenCode have been tested only with stand-ins and fixtures, never with the real agents. Nothing is called "supported" yet: see [Test results](#test-results) for exactly what was covered.

## Before you install

Six things to know first.

**1. It restarts agents without asking, with the flags they had.** After a reboot, nobody is at the keyboard when the agents start. By default herdr-wake restores the flags each agent was started with, **including permission-bypass flags** such as Claude Code's `--dangerously-skip-permissions`, Codex's `--dangerously-bypass-approvals-and-sandbox` (`--yolo`), Pi's `--approve` and OpenCode's `--auto`. If you do not want that, put this in `~/.config/herdr-wake/config.toml`:

```toml
restore_bypass = false
```

It takes effect at the next wake, for sessions already recorded too. With it, these flags are left out of the resume command:

| Agent | Dropped when `restore_bypass = false` |
|---|---|
| Claude Code | `--dangerously-skip-permissions`, `--allow-dangerously-skip-permissions`, `--permission-mode` with any mode other than `default`, `manual`, `plan` or `dontAsk`, `--settings`, `--allowed-tools` / `--allowedTools`, `--tools` |
| Codex | `--dangerously-bypass-approvals-and-sandbox`, `--yolo`, `--approve-for-me`, `--not-so-yolo`; `-s` / `--sandbox` unless its value is `read-only` or `workspace-write`; `-a` / `--ask-for-approval` unless its value is `untrusted`, `on-request` or `on-failure`; every `-c` / `--config` whose key (or a dotted part of it) is `approval_policy`, `sandbox_mode`, `profile` or starts with `sandbox_`; `-p` / `--profile` |
| Pi | `-a` / `--approve` |
| OpenCode | `--auto` and its aliases `--yolo` and `--dangerously-skip-permissions` |

The switch covers command-line flags only. It does not touch what an agent's own config files allow (a permissive `~/.claude/settings.json`, a Codex `config.toml` with `approval_policy = "never"`, a project's settings). See [docs/LIMITS.md](docs/LIMITS.md#flags-and-permissions).

**2. It becomes the program every recovered terminal starts with.** herdr-core has a `shell` setting: the program it starts in a terminal that has no command of its own, including every terminal it recovers after a restart. `herdr-wake install` sets it to `herdr-wake shell`. That program reads the terminal's record, resumes the agent if there is one, and then hands over to your normal shell. Any error also ends in your normal shell.

If the herdr-wake binary is deleted without running `herdr-wake uninstall` first, herdr-core cannot start those terminals at all. It retries forever and shows no error. To recover, do one of these:

- Put the binary back (into the same folder; set `HERDR_WAKE_BIN_DIR` if you used another) and let herdr-wake repair or remove itself:

  ```sh
  curl -fsSL https://github.com/Heretikio/herdr-wake/releases/latest/download/install.sh | HERDR_WAKE_NO_SETUP=1 sh
  herdr-wake doctor      # shows what is wrong and how to fix it
  herdr-wake uninstall   # only if you want it gone
  ```

- Or edit `~/.config/herdr-core/config.toml` (or `$XDG_CONFIG_HOME/herdr-core/config.toml`) by hand. Find the line

  ```toml
  shell = ["/home/you/.local/bin/herdr-wake", "shell"]
  ```

  and either delete it (core then uses its default shell) or replace it with the value saved as `real_shell` in `~/.config/herdr-wake/config.toml`. herdr-core picks up the change within about 60 seconds, without a restart.

**3. On a Mac, sessions come back with the first connection, unless you set up more.** The default boot service is a LaunchAgent, which runs when someone logs in at the Mac's screen (or at boot, if auto-login is on). What you get by default, with auto-login off: after a reboot nothing runs until the first connection starts herdr-core (the Herdr desktop app on another machine, or an SSH session that runs `hc`; whether the phone app's connection starts it has not been tested), and herdr-wake then resumes the sessions within seconds. That case has been tested: with the Mac at its login screen and the Herdr desktop app on another machine as the first connection, two real Claude Code sessions were back within four seconds. If you install over SSH while the Mac sits at its login screen, the LaunchAgent is loaded at the next login.

To have sessions running before any connection, use one of these (neither has been reboot-tested yet):

- Turn on auto-login, so the LaunchAgent runs at boot. FileVault makes auto-login unavailable.
- `herdr-wake install --before-login` prepares a LaunchDaemon instead, which starts herdr-core at boot before anyone logs in. Installing it needs root, so herdr-wake stages the file and prints the `sudo` commands for you to run yourself (see point 5). With FileVault on, the daemon waits until the disk is unlocked.

Signing in: until someone logs in at the Mac's screen, the macOS login keychain is locked. Claude Code stayed signed in at the login screen when its credentials were in `~/.claude/.credentials.json` (seen in the Mac test). When they are only in the keychain, a resumed Claude session may come up signed out (not seen in testing; this comes from Anthropic's documentation). Workarounds: a token from `claude setup-token` in `CLAUDE_CODE_OAUTH_TOKEN`, or Claude's file-based credentials.

On Linux, `--before-login` has no effect: install always tries to turn on linger, so the user service starts herdr-core at boot with nobody logged in (tested on Fedora 44).

**4. A resumed agent loads its folder's configuration again, with nobody watching.** Project-level hooks, MCP servers and instruction files (`.claude/settings.json`, `.mcp.json`, `AGENTS.md`, `.pi/`, `opencode.json`) in the session's folder run again at every boot. So herdr-wake resumes, and records, only in a folder that is yours alone: owned by you, not writable by others (group-writable is accepted only when the group is your own private group, as on Fedora, Debian and Ubuntu), not a symlink, with no folder above it that is world-writable without the sticky bit or owned by another user. A private folder inside `/tmp` passes; a folder writable by a shared group, or one inside a colleague's tree, does not. Such a session is not recorded, and one recorded earlier ends in a plain shell with the line `folder is not safe to resume in: <why>`. Do not leave sessions running in repositories you do not trust: use `exclude_dirs`, `herdr-wake off <terminal>` or `herdr-wake forget <terminal>`.

Anyone who can use your herdr-core (every paired app and connected machine) can type into the resumed agents, as into any terminal there. What herdr-wake starts at boot is not theirs to choose through a label: a record is resumed only when it matches the private copy herdr-wake itself kept on this machine (see [Security](#security)).

**5. Never run herdr-wake with `sudo`.** Everything installs for your user. `install`, `uninstall`, `adopt`, `resume`, `forget`, `off` and `on` refuse to run as root. The one step that needs root is the optional `--before-login` LaunchDaemon on macOS: herdr-wake prints those `sudo` commands for you to read and run. The installed daemon is a root-owned file that runs herdr-wake as you.

**6. Three things the real reboot tests showed.**

- **Keep agent sessions in project folders, not your home folder.** Claude Code asks "do you trust this folder?" for a folder it has not been told to trust, and for the home folder it asks every time. After a reboot nobody is there to answer, so the resumed agent waits at that prompt (the conversation is there once you answer) and herdr-wake reports the terminal as `unconfirmed`. In the Mac test, the session in a project folder came back with no prompt; that trusting a folder once is always enough has not been checked further.
- **Mode changes made inside an agent are not restored.** herdr-wake restores the flags an agent was started with. If you switch Claude Code to auto mode (or plan mode) with shift+tab during a session, the resumed agent starts in the mode its launch flags give it.
- **Terminals created with `hc run` resume, but the Herdr desktop app does not draw them.** The desktop app shows only terminals that carry its own space and tab labels, which a terminal made from the command line does not have. The phone app lists them.

## What happens after a reboot

![While the machine is up, an agent hook records the session as a label on its herdr-core terminal. After a reboot, the boot service starts herdr-core, which recreates the terminals; herdr-wake runs in each one, resumes the agent, then starts your normal shell.](docs/assets/flow.svg)

1. **While the machine is up**, each agent's hook or plugin calls `herdr-wake hook`. herdr-wake stores the session id, folder and launch flags as a label on that herdr-core terminal, and keeps a private copy of the same facts in `~/.local/state/herdr-wake/records/`. Sub-agents and one-shot runs (`claude -p`, `codex exec`) are skipped.
2. **The machine reboots.** herdr-core keeps the label with the terminal.
3. **After the reboot**, the boot service runs `herdr-wake boot`, which starts herdr-core (on a Mac with auto-login off, the first connection starts it instead; see point 3 above). Core recreates each kept terminal and starts `herdr-wake shell` in it. Where there is a live record that matches herdr-wake's own copy, the agent is resumed. When it exits (or at once, if there is nothing to resume), your normal shell takes over in the same terminal.

Only agents that were still running when the machine went down come back. A session you quit stays closed. A terminal that never had an agent gets a plain shell. Starts are staggered: the first agent of each kind starts alone, the rest follow 1.5 seconds apart, so several Claude sessions do not race for the same login token.

## Agents and platforms

Nothing here is labeled "supported" yet: that label needs a pass of the full real reboot test in [docs/TESTING.md](docs/TESTING.md#the-real-reboot-matrix). "Reboot-tested" means a real reboot brought the real agent back, within the limits stated next to it (see the next section).

| Agent | Minimum version | How it is recorded | Status |
|---|---|---|---|
| Claude Code | none set (flag rules built from 2.1.322) | `SessionStart` and `SessionEnd` hooks in `settings.json` | reboot-tested 2026-11-10 on Linux x86_64 and macOS arm64 (Mac: login-screen case only); see [Test results](#test-results) |
| Codex CLI | none set (rules built from Codex source, 2026-11) | `SessionStart` hook in `hooks.json` (approve it once in `/hooks`) | automated tests only (recorded fixtures and a stand-in; the real Codex has never been run with herdr-wake) |
| Pi | 0.79.4 | extension in `~/.pi/agent/extensions/` | automated tests only (fake host and a stand-in; the real Pi has never been run with herdr-wake) |
| OpenCode | 2.0.28 for v2 (v1 handled, no floor known) | TUI plugin | automated tests only (fake host and a stand-in; the real OpenCode has never been run with herdr-wake) |

| Platform | Status | What has run there |
|---|---|---|
| macOS, Apple silicon | Claude Code reboot-tested 2026-11-10, login-screen case only | Full suite: 593 tests, including 23 end-to-end tests against a real herdr-core 0.1.20. One real reboot (macOS 27.2, herdr-core 0.1.18, auto-login off) where the first connection was the Herdr desktop app on another machine. The LaunchAgent at login or with auto-login, and `--before-login`, have not been reboot-tested |
| macOS, Intel | not tested | A release build is produced; no tests have run on Intel |
| Linux, arm64 | automated tests only | Full suite: 595 tests, including the same 23 end-to-end tests against a real herdr-core 0.1.20 and the Linux-only platform tests. No real reboot |
| Linux, x86_64 | Claude Code reboot-tested 2026-11-10 | Two real reboots of a Fedora 44 machine (bare metal, systemd, herdr-core 0.1.18): one with a stand-in agent, one with real Claude Code plus stand-ins. `cargo clippy` for this target; `doctor` and `status` run by hand. The test suite has not run on x86_64 hardware (CI is set up for Ubuntu x86_64 but has not run yet) |
| Windows | coming soon | Nothing to install yet |
| iOS and Android | nothing to install on the phone | The phone app connects to herdr-core on your machine; herdr-wake runs there |

herdr-core 0.1.16 or newer is required (the first version with the `shell` setting). `herdr-wake doctor` checks the core version. herdr-wake does not work with the Herdr TUI (`herdr`), which resumes agents by itself.

## Test results

The rule for this project: an agent and OS pair is "supported" only after it passes the full real reboot test: real hardware, the real agent, nobody at the keyboard, and the sessions coming back by themselves. The procedure is in [docs/TESTING.md](docs/TESTING.md#the-real-reboot-matrix), and the full record of every run, newest first, is in [docs/TEST_RESULTS.md](docs/TEST_RESULTS.md). No pair has passed the full test yet. The tests run so far each covered part of it, as the Result column says; every other cell reads "not yet run".

| # | OS | Agent | First connection after boot | Result | Date | Versions (herdr-wake, herdr-core, agent) |
|---|---|---|---|---|---|---|
| 1 | macOS | Claude Code | SSH | not yet run | | |
| 2 | macOS | Codex | SSH | not yet run | | |
| 3 | macOS | Pi | SSH | not yet run | | |
| 4 | macOS | OpenCode | SSH | not yet run | | |
| 5 | Linux x86_64 (Fedora 44) | Claude Code | none: the boot service started herdr-core before login, with nobody connected | resumed by itself; then waited at Claude Code's folder trust prompt (home folder), conversation intact once answered | 2026-11-10 | 0.1.0, 0.1.18, 2.1.304 |
| 6 | Linux | Codex | SSH | not yet run | | |
| 7 | Linux | Pi | SSH | not yet run | | |
| 8 | Linux | OpenCode | SSH | not yet run | | |
| 9 | macOS | Claude Code | Herdr phone app | not yet run | | |
| 10 | Linux | Claude Code | Herdr phone app | not yet run | | |
| 11 | macOS arm64 (27.2, auto-login off, at the login screen) | Claude Code | Herdr desktop app on another machine, which started herdr-core | pass: two real sessions resumed signed in, within four seconds of that connection | 2026-11-10 | 0.1.0, 0.1.18, 2.1.322 |

Row 11 is not one of the ten planned tests: it covers the Mac's default setup with auto-login off, where the first connection starts herdr-core. The LaunchAgent at a desktop login or with auto-login, and `--before-login`, have not been reboot-tested.

What has been verified so far, on 2026-11-10:

- **Three real reboots.** Two on a Fedora 44 x86_64 machine: the first with a stand-in agent (it confirmed that the boot service starts herdr-core with nobody logged in, that running agents come back with their flags and session ids, that a quit agent and a plain shell stay as they were, and that the shutdown is not mistaken for a quit); the second with one real Claude Code session and three stand-ins, all four started again by herdr-wake (the real one then waited at the trust prompt, see point 6 under [Before you install](#before-you-install)). One on an Apple silicon Mac at its login screen, with two real Claude Code sessions, one of them created from the Herdr desktop app. After the Mac test the phone app listed a resumed session, but only after the desktop app had connected: the phone app as the first connection has not been tested.
- **Automated tests.** 593 pass on macOS arm64 and 595 on Linux arm64. 23 of them are end-to-end: they drive a real herdr-core 0.1.20 in a throwaway home folder, with small scripts standing in for the four agents. Stopping and starting that core stands in for a reboot.
- **The security fixes** ([docs/SECURITY_FIXES.md](docs/SECURITY_FIXES.md)) were in the builds used for the second Linux reboot and the Mac reboot, so the record copies and the folder checks have been through real reboots. Upgrading from the earlier build was tried: its records showed as `untrusted` until accepted with `herdr-wake resume <terminal>`. No fix has been exercised by a real second user account, a real `sudo` run or a real LaunchDaemon install.
- **Codex, Pi and OpenCode have never been run for real with herdr-wake.** The Pi and OpenCode plugins have run only against fake hosts in Node. Codex hook handling, and the way herdr-wake works out which terminal a Codex session belongs to when Codex runs its shared daemon, have been tested only against recorded fixtures.

## How it works

herdr-wake is one self-contained binary (Rust, no Node or Python at runtime). It has three jobs:

- **Record.** The agent's own hook or plugin runs `herdr-wake hook <agent> <event>` with a JSON payload. herdr-wake keeps a record as a label (`wake`) on the herdr-core terminal: agent, session id, folder, launch flags, a short allowlist of environment variables, and the agent's process. It keeps the parts that decide what a resume would run in a private file of its own as well, and later acts only on a label that still says the same. For an agent you started by hand it also starts a small detached watcher, so a deliberate quit is noticed and the session is not resumed.
- **Start core at boot.** A launchd job (macOS) or systemd user service (Linux) runs `herdr-wake boot`, which runs `hc server start` and waits for the resumes to settle. It writes a summary ("resumed", "failed", "skipped", "unconfirmed") that `herdr-wake status` shows.
- **Resume.** herdr-core runs `herdr-wake shell` in each recovered terminal. It reads the record, checks it (the local copy, the session id's shape, the folder, the program), rebuilds the resume command from the agent's current rules (it never replays a cached command line), waits its turn, runs the agent as the terminal's foreground job, and then replaces itself with your normal shell.

If a resume fails quickly, it is tried up to 3 times per boot with a 2-second pause. Failures that cannot change on retry (folder gone, session not found) stop at once. In every failure case you get your normal shell and one line like this, with the exact command to run:

```
herdr-wake: could not resume claude session 3f2a...: session not found. To retry: cd /home/you/project && /home/you/.local/bin/claude --resume 3f2a...
```

The full design, including what the prototype changed, is in [docs/DESIGN.md](docs/DESIGN.md). The decisions behind it are in [docs/DECISIONS.md](docs/DECISIONS.md).

## Commands

```
$ herdr-wake --help
Starts herdr-core at boot and resumes coding-agent sessions after a reboot

Usage: herdr-wake <COMMAND>

Commands:
  install    Set up agent hooks, herdr-core's shell setting and the boot service
  uninstall  Undo install: restore herdr-core's shell, then remove hooks and the boot service
  status     List recorded agent sessions and whether each is running
  doctor     Check the setup and say how to fix what is wrong
  forget     Remove a terminal's record so its session is not resumed
  off        Switch herdr-wake off for one terminal: forget its record, record nothing there
  on         Switch herdr-wake back on for a terminal that was switched off
  resume     Let a failed session resume again, and print the command to run it now
  adopt      Record agents that were already running before herdr-wake was installed
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

Terminals are named the way herdr-core names them, for example `t3`.

- `herdr-wake install [--before-login] [--dry-run] [--host NAME]` sets everything up (see [What install changes](#what-install-changes)). `--host` copies this binary to an SSH host and runs install there; the host must have the same OS and CPU.
- `herdr-wake uninstall [--dry-run]` undoes install.
- `herdr-wake status [--json]` lists recorded sessions and the last boot summary. It never starts herdr-core. States: `running`, `ended` (you quit it), `failed: <reason>`, `untrusted` (the label does not match herdr-wake's local copy, so it is not resumed) and `off`.
- `herdr-wake doctor [--json]` checks the setup, prints `ok`, `warn` or `FAIL` per check with a `fix:` line, and exits 1 when a check fails. It never starts herdr-core.
- `herdr-wake forget t3` drops the record, so that session is not resumed. The next agent started in `t3` is recorded again.
- `herdr-wake off t3` switches herdr-wake off for that terminal: it drops the record, and from then on nothing started there is recorded or resumed. `herdr-wake on t3` switches it back on.
- `herdr-wake resume t3` re-arms a session that failed, so it resumes at the next recovery, and prints the command to run it now. It does not start the agent itself. It also accepts a record shown as `untrusted`: read the command it prints before you rely on it.
- `herdr-wake adopt` records agents that were already running before you installed. It can do this for Claude Code only; restart Codex, Pi and OpenCode sessions once to record them.

`hook`, `watch`, `shell` and `boot` are internal: agents, herdr-core and the boot service call them.

<details>
<summary>Full help for each command</summary>

```
$ herdr-wake install --help
Set up agent hooks, herdr-core's shell setting and the boot service.

Only agents that are installed are set up: an agent counts as installed when its config folder exists or its program is on PATH. The others are skipped and nothing is created for them; run install again after installing one. Every file it changes is backed up first under ~/.local/state/herdr-wake/backups. The boot job starts as soon as it is installed, so herdr-core starts now if it is not already running.

Run it as your own user, never as root or under sudo.

Usage: herdr-wake install [OPTIONS]

Options:
      --before-login
          Start herdr-core at boot, before anyone logs in.
          
          macOS: prepares a LaunchDaemon instead of a LaunchAgent and prints the sudo commands for you to run (herdr-wake itself never runs as root). Claude Code may come up signed out before login, because its login sits in the Keychain.
          Linux: has no effect. Install always tries to turn on linger, which already starts the user service at boot.

      --dry-run
          Show what would change and write nothing

      --host <NAME>
          Copy this binary to an SSH host and run install there

  -h, --help
          Print help (see a summary with '-h')
```

```
$ herdr-wake uninstall --help
Undo install: restore herdr-core's shell, then remove hooks and the boot service

Usage: herdr-wake uninstall [OPTIONS]

Options:
      --dry-run  Show what would change and write nothing
  -h, --help     Print help
```

```
$ herdr-wake status --help
List recorded agent sessions and whether each is running.

States: running, ended (you quit it), failed: <reason>, untrusted (the record does not match herdr-wake's local copy, so it is not resumed; `herdr-wake resume <terminal>` accepts it, `herdr-wake forget <terminal>` drops it), off (switched off with `herdr-wake off`).

Reads herdr-core's socket and never starts herdr-core.

Usage: herdr-wake status [OPTIONS]

Options:
      --json
          Print JSON instead of a table

  -h, --help
          Print help (see a summary with '-h')
```

```
$ herdr-wake doctor --help
Check the setup and say how to fix what is wrong.

Never starts herdr-core. Exits 1 when a check fails.

Usage: herdr-wake doctor [OPTIONS]

Options:
      --json
          Print JSON instead of text

  -h, --help
          Print help (see a summary with '-h')
```

```
$ herdr-wake forget --help
Remove a terminal's record so its session is not resumed

Usage: herdr-wake forget <TERMINAL>

Arguments:
  <TERMINAL>  The terminal, for example t3

Options:
  -h, --help  Print help
```

```
$ herdr-wake off --help
Switch herdr-wake off for one terminal.

Removes the terminal's record and marks the terminal (label wake.off=1 in herdr-core): from then on no agent started there is recorded and nothing is resumed there. An agent that is running in it keeps running. `herdr-wake on <terminal>` undoes it; `herdr-wake status` shows such terminals as off.

Usage: herdr-wake off <TERMINAL>

Arguments:
  <TERMINAL>
          The terminal, for example t3

Options:
  -h, --help
          Print help (see a summary with '-h')
```

```
$ herdr-wake on --help
Switch herdr-wake back on for a terminal that was switched off.

Removes the mark that `herdr-wake off` set. The next agent started in the terminal is recorded again. An agent that is already running there is not recorded until it starts again (or, for Claude Code, until `herdr-wake adopt`).

Usage: herdr-wake on <TERMINAL>

Arguments:
  <TERMINAL>
          The terminal, for example t3

Options:
  -h, --help
          Print help (see a summary with '-h')
```

```
$ herdr-wake resume --help
Let a failed session resume again, and print the command to run it now.

Resets a record that failed or used up its attempts, so the terminal resumes at the next recovery (after a reboot or a herdr-core restart). It does not start the agent itself: it prints the command to run in that terminal now.

It also accepts a record that `herdr-wake status` shows as untrusted. Read the printed command first: you are saying that record is yours.

Usage: herdr-wake resume <TERMINAL>

Arguments:
  <TERMINAL>
          The terminal, for example t3

Options:
  -h, --help
          Print help (see a summary with '-h')
```

```
$ herdr-wake adopt --help
Record agents that were already running before herdr-wake was installed.

Looks at the foreground program of every herdr-core terminal. Claude Code sessions are recorded from Claude's own session files. Codex, Pi and OpenCode cannot be identified from outside: restart those agents once to record them.

Usage: herdr-wake adopt

Options:
  -h, --help
          Print help (see a summary with '-h')
```

</details>

### What `doctor` checks

herdr-wake's own `config.toml` (a file that cannot be parsed is a `FAIL` with the parser's message), herdr-core's `shell` setting (present, absolute, the file exists and is executable), the saved real shell, whether core is running and answers, the core version (0.1.16 or newer), kept terminals that have had no program for more than 5 seconds (what a broken `shell` setting looks like), the boot service, `boot.env` and whether its `PATH` covers every recorded agent, then per OS: FileVault, auto-login and when agents will come back (macOS); systemd as PID 1, linger, the user unit, and WSL (Linux). Per agent: Claude's hooks and whether `disableAllHooks` or `allowManagedHooksOnly` blocks them, Codex's hook and whether hooks are switched off, Pi's extension, OpenCode's plugin entry. When `pi --version` or `opencode --version` can be read (3 seconds each), a Pi older than 0.79.4 or an OpenCode 2.x older than 2.0.28 is a `warn`; an agent that is not installed gets no version line.

The Codex check always ends with a `warn` reminding you to approve the hook in Codex's `/hooks`, because herdr-wake cannot read that approval reliably. If you do not use Codex, ignore it.

## Configuration

Settings live in `~/.config/herdr-wake/config.toml` (or under `$XDG_CONFIG_HOME`). Every key is optional; a missing file means all defaults. Install creates the file to save `real_shell`.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Master switch. `false`: every terminal wakes to a plain shell, nothing is resumed and nothing new is recorded. |
| `restore_bypass` | `true` | Restore permission-bypass flags on resume (see [Before you install](#before-you-install)). `false` drops them, for sessions already recorded too. |
| `attempts` | `3` | Resume attempts per terminal per boot. A start that exits non-zero within 5 seconds counts as one failed attempt. |
| `stagger_ms` | `1500` | Gap between agent starts after the first of each kind, in milliseconds. |
| `first_alone_ms` | `8000` | How long the first agent of each kind starts alone, in milliseconds. The wait ends early once that agent reports in. |
| `exclude_dirs` | `[]` | Folders (and everything under them) whose sessions are never recorded and never resumed. Absolute paths, or paths starting with `~/`. Whole folder names are compared: `~/private` does not cover `~/private-notes`. |
| `real_shell` | written by install | herdr-core's `shell` setting as it was before install, as a list, for example `["/bin/zsh", "-l"]`. herdr-wake hands over to it. When absent, it uses `$SHELL` as a login shell, then `/bin/sh`. Uninstall puts it back. |

Example:

```toml
restore_bypass = false
exclude_dirs = ["~/scratch", "/home/you/clients/acme"]
```

Run `herdr-wake doctor` after editing. A file that cannot be parsed shows up as `FAIL  config:` with the parser's message. While the file is broken, nothing is resumed (every terminal wakes to a plain shell, with one line in the log) and nothing is recorded: herdr-wake does not fall back to the defaults, because the defaults restore bypass flags.

To stop one terminal for good, use `herdr-wake off <terminal>` (`on` undoes it). To drop one record only, use `herdr-wake forget <terminal>`. To stop everything, set `enabled = false`.

Environment variables, for unusual setups: `HERDR_WAKE_HC` points `herdr-wake boot` at a specific `hc`. The installer reads `HERDR_WAKE_VERSION`, `HERDR_WAKE_BIN_DIR`, `HERDR_WAKE_NO_SETUP`, `HERDR_WAKE_DRY_RUN` and `HERDR_WAKE_REPO` (see the top of [scripts/install.sh](scripts/install.sh)).

## What install changes

`curl ... | sh` downloads the release archive for your OS and CPU, checks its SHA-256 against the `.sha256` file from the same release, puts the binary at `~/.local/bin/herdr-wake` (a `mktemp` file in that folder, then an atomic rename; on macOS it clears the quarantine flag), and runs `herdr-wake install`. It never uses sudo and refuses to run under sudo. The checksum catches a damaged download, not a replaced release; see [Security](#security) for how to check where a build came from.

`herdr-wake install` then makes these changes, in this order. Every file it changes is backed up first to `~/.local/state/herdr-wake/backups/`, once per file per run, as it was before the run. Backups are private (mode 0600) but hold whatever the file held, API keys in an agent's settings included, and are kept until you delete them. Edits to other tools' files add or remove only herdr-wake's own entries and keep comments and order. `--dry-run` prints the list and writes nothing.

1. **`~/.config/herdr-wake/boot.env`**: `PATH`, `LANG`, `LC_*`, `SHELL`, `USER`, `LOGNAME` and `TMPDIR` from the shell you install from, so herdr-core started at boot sees the same programs you do. Install from a shell where your agents are on `PATH`.
2. **`~/.config/herdr-wake/config.toml`**: saves herdr-core's current `shell` setting as `real_shell`.
3. **Agent integrations, for each agent that is installed.** An agent counts as installed when its config folder already exists or its program is on your `PATH`. For the others install prints `skipped <agent>: not installed` and creates nothing, not even a folder. Run `herdr-wake install` again after you install another agent: it picks up the new one and changes nothing else.
   - Claude Code: a `SessionStart` and a `SessionEnd` hook in `~/.claude/settings.json` (or `$CLAUDE_CONFIG_DIR/settings.json`).
   - Codex: a `SessionStart` hook in `~/.codex/hooks.json` (or `$CODEX_HOME/hooks.json`). Codex runs it only after you approve it once: open Codex, type `/hooks`, trust the herdr-wake hook. The hook command never changes between versions, so the approval holds across upgrades.
   - Pi: `~/.pi/agent/extensions/herdr-wake.ts` (or under `$PI_CODING_AGENT_DIR`).
   - OpenCode: v2 gets `herdr-wake/tui.js` plus a `"./herdr-wake"` entry in `plugins` in `cli.json`; v1 gets `herdr-wake-tui.js` plus an entry in `plugin` in `tui.jsonc`. Both in `~/.config/opencode` (or `$OPENCODE_CONFIG_DIR`). The layout follows the installed version: `opencode --version` 2 or newer means v2. Only when the version cannot be read (OpenCode's folder exists but its program is not on `PATH`) does an existing `cli.json` decide.
4. **herdr-core's `shell` setting** in `~/.config/herdr-core/config.toml`: set to `["<absolute path>/herdr-wake", "shell"]`. This is done after the hooks, because a `shell` setting that cannot start leaves terminals dead. The path is the one you ran install through, never one that an upgrade removes: if herdr-wake runs from a versioned folder (Homebrew's `Cellar`, a `versions/` folder), install records a symlink to it from your `PATH` or the standard bin folders, and refuses, with an explanation, when there is none.
5. **The boot service**, which starts at once (so herdr-core starts now if it was not running):
   - macOS default: `~/Library/LaunchAgents/dev.heretik.herdr-wake.plist`, loaded into your login session. Its log goes to `~/Library/Logs/herdr-wake/boot.log`.
   - macOS with `--before-login`: `/Library/LaunchDaemons/dev.heretik.herdr-wake.<user>.plist` (root-owned). herdr-wake never writes it: install stages the plist privately in `~/.local/state/herdr-wake/` and prints the `sudo` commands under "NOT FINISHED. RUN THESE YOURSELF", starting with `sudo plutil -lint` on the staged file. `<user>` is your login name from the user database, not from `$USER`. The daemon names no log file; what the boot run did is in herdr-wake's own log.
   - Linux: `~/.config/systemd/user/herdr-wake.service`, enabled, plus `loginctl enable-linger` if linger was off (herdr-wake notes that it turned linger on). Its log is in `journalctl --user -u herdr-wake`.

herdr-wake's own log is `~/.local/state/herdr-wake/herdr-wake.log`; every boot run writes its summary there. Everything herdr-wake writes under `~/.config/herdr-wake` and `~/.local/state/herdr-wake` is private: folders 0700, files 0600, tightened at install if an older build left them looser, and never written through a symlink.

### Uninstall

```sh
curl -fsSL https://github.com/Heretikio/herdr-wake/releases/latest/download/uninstall.sh | sh
```

or `herdr-wake uninstall` followed by deleting the binary yourself. Uninstall runs in the reverse order:

1. Restores herdr-core's `shell` setting from `real_shell` (or removes the line), then reads it back. If it still names herdr-wake, uninstall stops and changes nothing else.
2. Removes the boot service: unloads the LaunchAgent and moves its plist to the Trash; for the LaunchDaemon it prints the `sudo` commands; on Linux it disables and deletes the unit and turns linger off only if herdr-wake turned it on.
3. Removes herdr-wake's hook entries and plugin files from each agent's config (every agent, installed or not, and both OpenCode layouts), deletes any settings file install created that is now empty, and removes folders that are left empty by that (for example a `~/.codex` an older herdr-wake created for a Codex you never had). A folder that holds anything else stays.
4. Moves `~/.config/herdr-wake` to the Trash (macOS), or backs it up and deletes it (Linux).
5. Leaves the binary. `uninstall.sh` removes it only after `herdr-wake uninstall` succeeded (to the Trash on macOS). A binary in a Homebrew folder is left for Homebrew. The script runs the herdr-wake named by `HERDR_WAKE_BIN`, else the one in the install folder, and only then one found in an absolute folder of your `PATH` (it says which).

Logs and backups stay in `~/.local/state/herdr-wake/`. Record labels stay on terminals; nothing reads them once herdr-wake is gone. herdr-core can take up to 60 seconds to apply the restored `shell` setting, so a terminal being recovered at that moment may stay blank until then.

## Security

A security audit of the source was done on 2026-11-10: [docs/SECURITY_AUDIT.md](docs/SECURITY_AUDIT.md). What was fixed, how, and what was not: [docs/SECURITY_FIXES.md](docs/SECURITY_FIXES.md). The fixes are covered by automated tests; no outside party has reviewed them.

- **Unattended starts.** Read [Before you install](#before-you-install), points 1 and 4. Anyone who can make your machine reboot causes your agents to start again with the flags they had.
- **Only herdr-wake's own records are resumed.** A terminal label can be written by anything that can reach your herdr-core socket (every paired app and connected machine). herdr-wake therefore keeps its own copy of each record in `~/.local/state/herdr-wake/records/` and resumes only a label that matches it on agent, session id, session file, folder, flags, environment and program, for the same terminal. Anything else gets a plain shell, a log line, and the state `untrusted` in `herdr-wake status`. `herdr-wake resume <terminal>` accepts such a record after you have looked at it; `herdr-wake forget <terminal>` drops it.
- **What a record may make herdr-wake run.** Even a matching record is checked at wake: the session id must be a plain id (letters, digits, `.`, `_`, `-`), the program must be an absolute path to the agent's own executable, owned by you or root and not writable by others, the folder must be yours alone (point 4), the flags go through the agent's allowlist again, and the environment through the allowlist below.
- **What is stored.** Records hold the session id, folder, launch flags, the agent's binary path and an allowlist of environment variables: `PATH` (absolute folders only), `LANG`, `LC_*`, `XDG_*_HOME` and the agents' own folder settings (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_DIR`, `OPENCODE_CONFIG_DIR` and a few more). Any name ending in `_KEY`, `_TOKEN`, `_PASSWORD` or `_SECRET` is never stored.
- **Secrets on a command line are left out, as far as herdr-wake can tell.** Never stored: inline JSON given to Claude's `--settings`, `--mcp-config` or `--agents` (a file path for the same flag is kept), a Codex `-c` override whose key or value contains `key`, `token`, `secret`, `password`, `auth` or `bearer`, and any flag value holding a URL with a user name or password (OpenCode's `--server` keeps its address and loses the password). The log names the flag that was left out; the resumed session starts without it. Other flag values are stored as you typed them and are visible to every client of your herdr-core, so do not put secrets on an agent's command line.
- **No network.** The binary makes no network requests. Only the install script downloads, over HTTPS, from GitHub releases. Its checksum guards against a damaged download, not against a tampered release. Releases built by the workflow in this repository carry a build provenance attestation: `gh attestation verify herdr-wake-<version>-<target>.tar.gz --repo Heretikio/herdr-wake`. No release has been published yet, so this has not been exercised.
- **No root.** See point 5 under [Before you install](#before-you-install).
- **Private files.** herdr-wake's config, log, records and backups are readable by you only (0700 folders, 0600 files).
- **Nothing in shell history.** The resumed agent is started directly, not typed into your shell.
- **Hooks fail quietly.** `herdr-wake hook` never prints and always exits 0, so it cannot break your agent; problems go to the log. Text from records and hook payloads is stripped of control characters before it is printed or logged.
- **Codex trust.** herdr-wake never writes Codex's hook trust. You approve the hook yourself in `/hooks`.
- Report problems through [GitHub issues](https://github.com/Heretikio/herdr-wake/issues), or privately to the maintainer for anything sensitive.

## Limits

Every known limit and untested area is in [docs/LIMITS.md](docs/LIMITS.md). The ones most people hit:

- Real reboot tests so far cover Claude Code only, on Linux x86_64 and on a Mac at its login screen (see [Test results](#test-results)). Codex, Pi and OpenCode have not been run for real.
- Claude Code's folder trust prompt stops an unattended resume in a folder it does not trust (always in the home folder); mode changes made inside an agent with shift+tab are not restored; terminals made with `hc run` are not shown in the Herdr desktop app. See point 6 under [Before you install](#before-you-install).
- On a Mac with auto-login off, nothing is resumed until the first connection after a reboot (point 3).
- Sessions in a folder that others, or a shared group, can write are neither recorded nor resumed. Your own private group is accepted (the umask 002 setup on Fedora, Debian and Ubuntu); a folder writable by any other group, such as `staff` on macOS, is refused until you `chmod g-w` it.
- After upgrading from a build without the local copies, every recorded session shows as `untrusted` until its agent starts again, or you run `herdr-wake adopt` (Claude Code) or `herdr-wake resume <terminal>`.
- Codex: approve the hook once in `/hooks`; two Codex sessions started in the same folder at the same time cannot be told apart, and neither is recorded.
- OpenCode v2: if the session no longer exists, OpenCode opens a new chat; herdr-wake reports that as "unconfirmed" but cannot prevent it.
- `adopt` handles Claude Code only.
- If you kill an agent you started by hand while its shell keeps running, herdr-wake treats it as a quit and does not resume it.
- herdr-core releases every few days and has no stable API for this; a core update can break herdr-wake. Run `herdr-wake doctor` after updating core.

## How it compares

Many people have built agent resume for Herdr, and herdr-wake borrows from them. As of 2026-11-10:

| | herdr-wake | [herdr-revive](https://github.com/dmazlum/herdr-revive) | [herdr-recover](https://github.com/guillaume86/herdr-recover) | [herdr-agent-resume](https://github.com/thuanlm215/herdr-agent-resume) | Herdr TUI |
|---|---|---|---|---|---|
| Works with | herdr-core (desktop and phone apps) | Herdr desktop app (mod) | herdr-core | Herdr desktop app (mod) | the TUI's own server |
| Runtime needed | none (one binary) | Node 20 | shell scripts | Python 3.8 | none |
| Designed to work with no app running | yes | no | yes | no | n/a |
| Starts herdr-core at boot | yes (on a Mac: at login or with auto-login, or before login with `--before-login`) | no | no | no | n/a |
| Agents | Claude Code, Codex, Pi, OpenCode | Claude Code; Codex, Droid, Cursor, Pi, omp experimental | Claude Code, Codex, OpenCode 2, Pi | Claude Code, Codex, Grok, Antigravity, Pi, Kiro | 18 agents |
| Launch flags on unattended resume | restored, bypass included, opt-out | restored, bypass dropped | restored, bypass included (flag capture on Linux only) | not restored on macOS | not compared |

What is different about herdr-wake: it starts herdr-core at boot, it is a single binary with no runtime, it is built to run with no app open (so reconnecting from the desktop or the phone makes no difference), it treats four agents as first-class, and it publishes a dated reboot-test table and labels everything else untested.

If you use the Herdr TUI, you do not need any of these: it resumes agents natively (`[session] resume_agents_on_restore`). It does not share sessions with herdr-core, so it does not help with the desktop or phone apps.

If herdr-core starts carrying a resume reference itself ([herdr-core #15](https://github.com/herdrdev/herdr-core/issues/15)), herdr-wake will read it from core and drop its hooks, and keep the boot start, the quit-versus-kill rule, staggered starts, `status` and `doctor`.

## FAQ

**Do I need the Herdr app open?** No. herdr-wake needs only herdr-core, and the apps connect to it as usual. On Linux the boot service starts herdr-core with nobody connected (tested). On a Mac with auto-login off, herdr-core starts with the first connection after a reboot, such as the desktop app on another machine (tested) or SSH; the phone app as the first connection has not been tested (see point 3 under [Before you install](#before-you-install)).

**Do I install anything on my phone?** No. The phone app talks to herdr-core on your machine over SSH; herdr-wake runs on that machine.

**Will it bring back an agent I quit?** No. Only agents still running when the machine went down are resumed.

**What about a terminal with no agent?** It gets your normal shell, as before.

**Can I keep one terminal from resuming?** `herdr-wake off t3` switches herdr-wake off for that terminal until `herdr-wake on t3`. `herdr-wake forget t3` drops only its current record. For a whole folder, add it to `exclude_dirs`. To turn everything off, set `enabled = false`.

**`status` says a session is `untrusted`. What is that?** Its label in herdr-core does not match the copy herdr-wake kept, so herdr-wake will not act on it: something else wrote or changed the label, the copy is gone, or the record is from a build that kept no copies. If the session is yours, `herdr-wake resume t3` accepts it (it prints the command it would run); otherwise `herdr-wake forget t3`.

**A terminal says `folder is not safe to resume in`. Why?** Someone other than you could change that folder, or a folder above it, and with it the hooks and MCP servers a resumed agent loads. The line says which rule failed. Fix the permissions (`chmod go-w`), or move the project.

**A resume failed. Now what?** The terminal shows one line with the reason and the exact command to run. `herdr-wake resume t3` re-arms it for the next recovery, and `herdr-wake status` shows the state of every record.

**What about a herdr-core restart, not a reboot?** The same path runs: core recreates kept terminals and starts `herdr-wake shell` in each. Stopping and starting herdr-core is exactly what the end-to-end tests do.

**I installed herdr-wake while agents were running. Are they recorded?** Claude Code: run `herdr-wake adopt`. Codex, Pi and OpenCode: restart each once.

**Why does doctor always warn about Codex trust?** herdr-wake cannot read whether you approved its hook in Codex's `/hooks`. If you did, or you do not use Codex, ignore it.

**Where are the logs?** `~/.local/state/herdr-wake/herdr-wake.log`, which includes each boot run's summary. The boot job's raw output: `~/Library/Logs/herdr-wake/boot.log` on macOS with the default LaunchAgent (the `--before-login` daemon has none), `journalctl --user -u herdr-wake` on Linux.

**Can I install it on another machine from here?** `herdr-wake install --host NAME` copies this binary over SSH and runs install there. The other machine must have the same OS and CPU.

**Windows?** Coming soon. The installer says so and stops.

**Building from source?** See [CONTRIBUTING.md](CONTRIBUTING.md).

## Credits

herdr-wake stands on work by others:

- **[Herdr](https://herdr.dev) and herdr-core** by the herdrdev team, and the **Herdr TUI**'s own agent resume, which set the shape of the resume commands, the anchored Claude hook matcher, comment-preserving edits to agent settings, and the lesson from its issue #5547 (rebuild the resume command from facts, never replay a cached one).
- **[guillaume86/herdr-recover](https://github.com/guillaume86/herdr-recover)**: installing as herdr-core's `shell` so resume works with no app, restoring `CLAUDE_CONFIG_DIR` and `CODEX_HOME`, the OpenCode TUI-plugin approach and the Pi quit-versus-signal check.
- **[dmazlum/herdr-revive](https://github.com/dmazlum/herdr-revive)**: `doctor`, backups before every edit, careful flag capture, and the treatment of an ambiguous Claude `SessionEnd`.
- **[thuanlm215/herdr-agent-resume](https://github.com/thuanlm215/herdr-agent-resume)**: the claim lock, `adopt`, the watcher that forgets a session when the agent exits and its shell lives, and keeping the Codex hook command stable so its trust holds.
- **pine2D/herdr-session-resume** (matching Claude's own session files for `adopt`), **quinnjr/herdr-auto-resume** (shell identity by pid, start time and boot id), **manastalukdar/herdr-agent-resume-plus** (boot-id-aware resume), **cantona/herdr-revive** (dry runs), **dougcunha/herdr-mods** (state as herdr-core labels), and the other community resume tools whose single good ideas (one-at-a-time starts, flag re-checks, Codex disambiguation) shaped this one.

## License

MIT. See [LICENSE](LICENSE).
