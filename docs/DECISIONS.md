# herdr-wake: design decisions

Agreed by the maintainer on 2026-11-10 in a design interview. Change a decision only with the maintainer's agreement.

## What it is

herdr-wake makes coding-agent sessions come back by themselves after a reboot, inside herdr-core (the headless runtime behind the Herdr desktop and mobile apps). herdr-core recreates terminals as empty shells after a reboot and its maintainer has declined to build agent resume (herdr-core #13, closed not planned 2026-10-27).

## Decisions

1. **Own project.** Claude Code, Codex, Pi and OpenCode are all first-class.
2. **Three jobs.** (a) Start herdr-core at boot. (b) Record each agent session. (c) Resume it automatically with no app or UI running. Out of scope: layout restore (core does it), any UI, Herdr TUI support (the TUI resumes natively).
3. **Restore exactly what was running**, permission-bypass flags included. Stated plainly in the README. An opt-out setting exists for people who want bypass dropped.
4. **Resume only agents that were still running when the machine went down.** A session the user quit stays gone. A terminal that never had an agent is left alone.
5. **Strictly per machine.** No coordinator, no sync. The installer can target a remote host over SSH.
6. **Published under heretik on GitHub, MIT license.**
7. **One self-contained Rust binary per OS.** One-line install. No Node, Python or Git Bash needed.
8. **v1 platforms: macOS and Linux.** iOS and Android are covered by a reboot test where the phone is the first connection. Windows is "coming soon"; keep the core logic free of Unix-only assumptions.
9. **Resume trigger: herdr-wake registers as herdr-core's `shell` setting.** It starts the agent directly when the terminal has a session to resume, then hands over to the user's normal shell. Any error falls back to the plain shell. To be confirmed by the prototype.
10. **Boot timing.** Before login on Linux (systemd user service plus lingering). On macOS aim for before login on machines without FileVault; FileVault is a documented limit ("resumes at login or on first remote connection"), not fought in v1. `doctor` says which case a machine is in.
11. **The session-id source is a swappable part.** If herdr-core ever carries a resume reference (herdr-core #15), read it from core and drop the hooks. The maintainer posts on #15 after launch.
12. **Failure handling.** Up to 3 attempts per terminal per boot with a short pause. Failures that cannot change on retry (session not found, folder missing) skip straight to the end state. End state: plain shell, one line with the reason, the exact manual command. The record is kept until resumed or forgotten. Starts are staggered.
13. **"Supported" means a real reboot test passed.** Matrix: 4 agents x 2 OSes, plus one phone-first test per OS (10 tests). Dated results with versions live in the repo. Untested combinations are labeled untested. Nightly job runs against the newest herdr-core.
14. **Name: `herdr-wake`.**
15. **v1 features beyond the three jobs:**
    - Commands: `install`, `uninstall` (backups, dry-run), `status`, `doctor`, `forget <terminal>`, `resume <terminal>`, `adopt`, and a log file.
    - Per-terminal and per-folder opt-out, and a global off switch.
    - Skip sub-agents and one-shot runs (`claude -p`, `codex exec`). Follow the session id across `/clear`, `/resume` and compaction.
    - Saved flags re-checked against the agent's current `--help`. `CLAUDE_CONFIG_DIR` and `CODEX_HOME` restored. A lock so two terminals cannot resume one session. Refuse a folder that is world-writable or not owned by the user.
    - Nothing written to shell history.
    - A post-boot summary ("9 resumed, 1 failed") as a notification or status line.
    - Auto-"continue" for agents that were mid-task: opt-in, off by default.
    - Not in v1: snapshots, named spaces, layout restore, sub-agent resume.
16. **Codex.** Fully supported only if both known problems are solved (deliberate quit is indistinguishable from a kill; the Codex daemon reports the wrong terminal). Otherwise ship with stated limits, no release delay. Planned attack: detect a quit by whether the shell outlived the agent; match sessions from Codex's own session files; fall back to setting no-daemon mode with consent.
17. **Test hosts.** Two Apple silicon Macs, a Windows host (Linux via a VM or WSL there), and a Linux box or Raspberry Pi if needed.
18. **Launch.** Private until the reboot matrix passes. Then v0.1.0 with: the dated test table, one-line install, a paste-to-your-agent install prompt, one diagram, a credited comparison table, the `herdr-mod` GitHub topic. Announcements the maintainer approves and posts: herdr-core #15, herdr-core #13, Herdr Discord, and a heretik blog post. Homebrew and `cargo install` in the first week.

## Order of work

1. Research docs in `docs/research/`.
2. Prototype on real hosts to answer: do labels survive a reboot, does a phone connection start core, can core start before login on a Mac.
3. Module design (agents and platforms as the two swappable parts).
4. Implementation plan.
5. Build test-first; security audit before going public; reboot matrix.
6. README, install prompt, diagram, blog post.
