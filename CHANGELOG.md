# Changelog

All notable changes to herdr-wake. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **`herdr-wake resurrect`: bring back a closed session.** `herdr-wake resurrect [WORD] [--here | --folder DIR] [--agent NAME] [--limit N] [--json]` lists the sessions you closed or lost, newest first, numbered, with age, agent, folder, title, how it ended, a `!` where the flags include a permission bypass, and where the row came from. `herdr-wake resurrect <number or session id> [--print] [--fork]` checks the session the way a wake does (valid id, not running, folder exists and is safe, program is safe, the agent's session file exists, `restore_bypass`) and opens a new herdr-core terminal in its folder that resumes it, or prints `cd <folder> && <command>`. `--fork` resumes a Claude Code session as a copy. The recorded flags are replayed, bypass included, because you asked; `restore_bypass = false` still drops them. The new terminal joins the Herdr space of the terminal you run it from, else the space whose folder holds the session when the desktop app is open, else it has no space label (the phone app and `hc ls` show it, the desktop app does not). A session that is running is never listed: a live record, a Claude session file with a live process, a Codex rollout a running Codex holds open, an agent whose command line names it, or a session file written in the last two minutes. Numbers belong to the last table listed in the same terminal and last an hour; `--json` does not set them; `--id` names an id that looks like a number; `--fork` also works on a running Claude session. The new terminal takes its launch ticket by renaming it, so it runs once, and holds a run lock (pid and start time) from before the agent starts until it exits, so a second launch of the same session is refused even before the agent has registered itself. Flag values are not shown (names only, except `--permission-mode`, `--model`, `-s/--sandbox`, `-a/--ask-for-approval`) and credential-looking titles are hidden; a folder or flag with control or direction-changing characters is refused. `hc` and `herdr-app` must pass the program safety check and are called with time limits.
- **A history journal.** herdr-wake now keeps one private file per recorded session in `~/.local/state/herdr-wake/history/` (agent, session id, folder, the exact flags, program, environment allowlist, terminal, start and end, and how it ended: quit, killed, replaced, forgotten, unknown), written by the hook, the watcher, the wrapper, `forget` and `off`. `history_limit` in `config.toml` caps it (default 500, oldest activity pruned first, 0 keeps none).
- **Transcript fallback.** Sessions the journal does not know (from before install, or run outside Herdr) are read from Claude Code's and Codex's own transcripts: the first and last 128 KB of each file, cached by size and modification time. Flags there are rebuilt from Claude's first recorded permission mode and marked as from the transcript; Codex gets none.
- **Pi and OpenCode sessions that only left their own files** are listed too: Pi's session files (`~/.pi/agent/sessions`, honouring `PI_CODING_AGENT_DIR` and `PI_CODING_AGENT_SESSION_DIR`) and OpenCode's database (`opencode.db`, `OPENCODE_DB` and `XDG_DATA_HOME` honoured), read with `sqlite3 -readonly -json` (an argument list, 5 seconds, the program checked like `hc`). Without `sqlite3`, OpenCode rows are left out. Title, folder, activity time and how the conversation ended; sub-agent (`parent_id`) and archived OpenCode sessions are skipped. They resume with no flags beyond what the agent's own resume adds.
- **How the conversation ended, and details, in `resurrect`.** The ENDED column adds `finished`, `unanswered`, `interrupted` or `mid-work` for Claude Code, Codex, Pi and OpenCode rows (`quit, finished`; `stopped mid-work` for a transcript-only row), judged from your words and the agent's answers only (reminders, commands, hook output, sub-agent rows and compaction summaries do not count). `--json` gains `conversation`, `branch`, `cost_usd`, `duration_ms`, `started_ms`, `lines_added`, `lines_removed`, `models`, `prs` (`number`, `url`, `repo`; `https` only), `summary` and `summary_source`; `ending` stays the journal's. The whole file is read once per change, with a byte search deciding which lines are parsed, and kept in the transcript cache (`CACHE_VERSION` 4).
- **Summaries.** `herdr-wake resurrect --summary <ID>` prints `{summary, summary_source}` and `--prefetch-summaries <N>` makes the missing ones for the top N of the listing, one by one, stopping at the first failure of `claude` itself. Claude Code's own recap with no later message of yours is used as it is (`recap`); otherwise the conversation (words only, never over a million characters) goes to the local `claude -p` (no settings, MCP servers, slash commands, tools or session file, `dontAsk`, `claude-haiku-5-5`, low effort, 150 seconds, the session's own environment variables removed, `--bare` not used) and the answer is saved privately in `~/.local/state/herdr-wake/summaries/` until the session file changes. A plain listing only shows what is saved. `summaries = false` in `config.toml` stops generation. A summary that seems to hold a secret is stored and shown as hidden.
- **`doctor`** says how long Claude Code keeps transcripts (`cleanupPeriodDays`, 30 days by default) and that `resurrect` cannot bring back a session whose transcript is gone. Information only, never a failure.

- **A Sessions panel for the Herdr desktop app** (`mods/herdr-wake-sessions/`): the closed sessions in a side panel with scope switch, filter, session facts, Resume, Fork and Copy. New for it: `resurrect --space <SPACE-ID>` (the new terminal joins that space, ahead of the guesses), `resurrect --ticket` (checks and writes the launch ticket, opens nothing, prints `{key, argv, cwd, title, space}`), `transcript`, `folder_exists` and `ended_ms` in `--json`, and the tab `hc run` opens is renamed after the session title when the app is open.

- **The Sessions panel lists every Herdr machine.** Each connected machine of `hc machine ls` is asked in parallel (`hc exec --on`, an argument list, machine names checked, 15 seconds each, one slow or failing machine never blocks the others), with a machine picker in the header (All machines, This computer, each machine that answered), the machine at the start of a remote row's place line, in the filter and in the session's facts, and quiet footer lines for a machine without a herdr-wake that has `resurrect` or that is offline. Resume, Fork, the copy menu's resume command and summaries run on the session's own machine (the background summaries: newest 20 here, 5 per other machine). A session's tab opens in a space on its machine, and a machine with no space gets one.

### Fixed

- **The panel's own timers.** It used `setTimeout`, which the Herdr shell does not have (the refresh after a Resume failed with that message); it uses `cx.timer.after` now.
- **A relative `CLAUDE_CONFIG_DIR` in a recorded environment** is read under the session's folder by Claude's resume check, as Codex's `CODEX_HOME` already was (before, it was read under the folder of whoever ran the check).
- **Text from records and files** is also stripped of direction-changing characters (U+202A to U+202E, U+2066 to U+2069) and the line and paragraph separators before it is printed or logged.

## [0.1.2] - 2026-11-21

### Added

- **Resume through a wrapper.** `programs` in `config.toml` sets, per agent, the program to resume with instead of the recorded one, for agents started through a wrapper script (one that sets up the environment, then runs the agent). It must be owned by you or root, executable and not writable by group or others; its name is not checked, so `programs` is used only while `config.toml` and its folder are yours alone. `doctor` lists each entry and warns about a name that is not an agent. The flags and the session id are rebuilt as before. A step before the resume that runs the recorded program (OpenCode v2 starts its service) still checks that program as before. ([#4](https://github.com/Heretikio/herdr-wake/pull/4))

### Fixed

- **Debug builds aborted on some macOS accounts.** For a group from a directory service, macOS can place the group's member list at an unaligned address; the folder trust check read it with a plain read, which a debug build aborts on (and is undefined behaviour in a release build). It is now read without assuming alignment. ([#3](https://github.com/Heretikio/herdr-wake/pull/3))

## [0.1.1] - 2026-11-14

### Fixed

- **Codex 0.167 sessions were never recorded.** Codex 0.167 labels its own terminal sessions `source: "vscode"`, which herdr-wake read as the IDE extension and skipped as non-interactive. A `vscode` session now counts as interactive when its `originator` is the terminal client (`codex-tui` or `codex_cli_rs`); the IDE extension and the desktop app are still skipped. ([#1](https://github.com/Heretikio/herdr-wake/issues/1))
- **Flaky staggered-start test on macOS CI.** Start slots are fixed times, so one late start on a busy runner shortened the next gap below the test's floor. The test now requires each gap to clear half the stagger and the average to come within 10% of it. ([#2](https://github.com/Heretikio/herdr-wake/issues/2))

### Known issues

- **Codex 0.169 confirms a resume only at the first prompt.** It runs its start hook when the first message is sent, so after a reboot a resumed Codex is listed as unconfirmed until you type in it. Codex itself is back with its session. See [docs/LIMITS.md](docs/LIMITS.md).

### Tested

- Real reboot on macOS arm64 with real Codex 0.169: recorded, resumed into its session after the reboot. Record in [docs/TEST_RESULTS.md](docs/TEST_RESULTS.md).

## [0.1.0] - 2026-11-10

### Added

- **Boot start.** `herdr-wake boot` starts herdr-core when the machine starts, with no app or client running: a LaunchAgent at login on macOS (or a LaunchDaemon before login with `install --before-login`), a systemd user service with linger on Linux. It waits for the network (up to 30 s), runs `hc server start`, waits for the resumes to settle (up to 120 s) and writes a summary that `status` shows.
- **Recording** for Claude Code (`SessionStart`/`SessionEnd` hooks), Codex (`SessionStart` hook, matched to its terminal by foreground process so it works with Codex's shared daemon), Pi (extension) and OpenCode v1 and v2 (TUI plugin). Sub-agents and one-shot runs are skipped; the session id is followed across `/clear`, resume, fork and compaction.
- **Resume** through herdr-core's `shell` setting: `herdr-wake shell` resumes the recorded session in each recovered terminal as its foreground job, then hands over to the real shell. Any error ends in the real shell.
- **Quit detection.** A deliberate quit ends the record so the session is not resumed: from the agent's own end event, the exit status of an agent herdr-wake started, or a watcher that checks whether the shell outlived the agent. Shutdown detection on macOS (login window notifications) and Linux (logind) keeps records during a shutdown.
- **Flag restore** through per-agent flag tables. Permission-bypass flags are restored by default; `restore_bypass = false` drops every launch flag that loosens permissions (the list is in the README).
- **Failure handling.** Up to 3 attempts per terminal per boot, with a 2 s pause; permanent failures (folder or session gone) stop at once. The terminal shows one line with the reason and the exact command to run.
- **Staggered starts.** The first agent of each kind starts alone (up to 8 s), the rest 1.5 s apart. A per-boot claim stops two terminals from resuming the same session.
- **Commands:** `install` (with `--dry-run`, `--before-login`, `--host` for SSH), `uninstall` (with `--dry-run`), `status`, `doctor`, `forget`, `off` and `on` (per-terminal opt-out), `resume`, `adopt` (Claude Code only).
- **Settings** in `~/.config/herdr-wake/config.toml`: `enabled`, `restore_bypass`, `attempts`, `stagger_ms`, `first_alone_ms`, `exclude_dirs`, `real_shell`.
- **Safe edits.** Install backs up every file it changes, edits other tools' JSON, JSONC and TOML without losing comments or order, and uninstall restores herdr-core's `shell` setting first and verifies it before removing anything else.
- **Install and uninstall scripts** that verify the download's SHA-256, never use sudo, and keep the binary when `herdr-wake uninstall` fails.
- **Tests:** 593 automated tests on macOS arm64 and 595 on Linux arm64, 23 of them end-to-end against a real herdr-core 0.1.20 in an isolated home with stand-in agents.
- **Docs:** README, [docs/LIMITS.md](docs/LIMITS.md), [docs/TESTING.md](docs/TESTING.md), the paste-to-your-agent [install prompt](docs/INSTALL_PROMPT.md), and a flow diagram.

### Tested

Real reboots on 2026-11-10; the full record is in [docs/TEST_RESULTS.md](docs/TEST_RESULTS.md).

- **Linux x86_64 (Fedora 44, herdr-core 0.1.18), stand-in agent:** the boot service started herdr-core with nobody logged in; running agents came back with their flags and session ids, a quit agent and a plain shell stayed as they were. Found the clock step and the install-creates-folders bugs (fixed, see below).
- **Linux x86_64, real Claude Code 2.1.304:** resumed by herdr-wake before any login, then waited at Claude Code's folder trust prompt (home folder). The monotonic clock fix held through another 130 s clock step.
- **macOS 27.2 on Apple silicon, real Claude Code 2.1.322, auto-login off:** at the login screen, the Herdr desktop app on another machine was the first connection and started herdr-core; two real sessions (one created from the desktop app) were resumed, signed in, within four seconds.
- **Not tested:** real Codex, Pi or OpenCode; the Mac LaunchAgent at login or with auto-login; `--before-login`; the phone app as the first connection; a Linux arm64 reboot; the install script against a real release.

### Security

Changes after the security audit of 2026-11-10 ([docs/SECURITY_AUDIT.md](docs/SECURITY_AUDIT.md)); the full account is in [docs/SECURITY_FIXES.md](docs/SECURITY_FIXES.md).

- **Folder safety (M-1).** A session is neither recorded nor resumed in a folder that is not yours alone: not owned by you, writable by others or by a group other than your own private group, a symlink, or under a folder that is world-writable without the sticky bit or owned by another user. The terminal shows `folder is not safe to resume in: <why>`.
- **Records are checked, and only herdr-wake's own are resumed (M-5).** The hook keeps a private copy of each record; a label that differs from it is not resumed and shows as `untrusted` in `status` (`resume <terminal>` accepts it, `forget` drops it). At wake the session id's shape, the program, Pi's session file, the flags and the environment are validated again.
- **`restore_bypass = false` means it (M-2).** It now also drops Claude `--settings`, `--allowed-tools`, `--tools`, and Codex `--sandbox`, `--ask-for-approval`, `--profile` and `-c` overrides that loosen the sandbox or approvals.
- **Secrets stay out of the label (M-3).** Inline JSON for Claude `--settings`, `--mcp-config` and `--agents`, credential-looking Codex `-c` overrides and URLs with a password are never stored.
- **Never as root (M-4).** `install`, `uninstall`, `adopt`, `resume`, `forget`, `off` and `on` refuse to run as root. The macOS root mode is gone: the LaunchDaemon is always staged, and install prints the `sudo` commands.
- **Private files (L-6).** Everything under herdr-wake's config and state folders is written 0600 in 0700 folders, atomically, never through a symlink; backups too.
- **LaunchDaemon (L-7).** No launchd-opened log file, the user name comes from the user database, the plist is staged privately, and `sudo plutil -lint` comes first.
- **Scripts (L-9).** `install.sh` stages with `mktemp`; `uninstall.sh` runs `HERDR_WAKE_BIN` or the default install, never a `herdr-wake` from a relative `PATH` entry.
- **Output (L-10).** Control characters from records never reach the terminal or the log.
- **Release (L-8).** Every GitHub Action is pinned to a commit; the release job attests build provenance.

### Changed

- **Install sets up only agents that are installed** (config folder present, or program on `PATH`), prints `skipped <agent>: not installed` for the others and creates nothing for them. Run `herdr-wake install` again after installing another agent. Uninstall also removes folders it leaves empty.
- **OpenCode's layout at install follows `opencode --version`**; `cli.json` decides only when the version cannot be read.
- **One backup per file per run.**
- **Install never records a versioned path** (Homebrew `Cellar`, `versions/`) as herdr-core's shell; it uses a symlink to it or refuses.
- **A `config.toml` that cannot be parsed** now resumes nothing (it used to fall back to the defaults, bypass flags included); `doctor` reports it as a failed check.
- **`exclude_dirs`** understands a leading `~` everywhere, and an excluded session is not recorded.
- **`doctor`** warns about a Pi older than 0.79.4 or an OpenCode 2.x older than 2.0.28.
- **Timing uses a monotonic clock** (gate slots, stagger, the boot run's duration): a wall clock stepped back during boot no longer delays agents or gives the boot summary a negative duration. `last-boot.json` has a new `duration_ms`.
- **The boot run writes its summary to `herdr-wake.log`.**

### Removed

- **`auto_continue`.** It never did anything. A config file that still has the key loads.

### Requires

herdr-core 0.1.16 or newer. Pi 0.79.4 or newer. OpenCode 2.0.28 or newer for v2.
