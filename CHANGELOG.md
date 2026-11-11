# Changelog

All notable changes to herdr-wake. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

The first release, 0.1.0, is being prepared. It ships once the real reboot matrix has been run and its results are in the README.

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
