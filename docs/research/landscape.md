# Landscape: Herdr, herdr-core and the existing resume tools

Researched 2026-11-10 from herdr.dev, the herdrdev GitHub org, a Moshi comparison article and the READMEs and source of about 30 community projects. This is the background for why herdr-wake exists and how it is positioned. Counts (stars, versions) are as of that date.

## The three Herdr products

| Product | What it is | Version | Resumes agents after reboot? |
|---|---|---|---|
| Herdr TUI (`herdr`) | The original terminal app with its own server. github.com/herdrdev/herdr, about 50k stars | 0.9.5 | Yes, natively (`[session] resume_agents_on_restore`, 18 agents, since 0.9.2). Resume runs after a client attaches. |
| herdr-core (`hc`) | Headless terminal runtime. github.com/herdrdev/herdr-core, Apache-2.0, Rust, 593 stars, a release every 1 to 3 days | 0.1.20 | No. Kept terminals come back as empty shells. |
| Herdr app (desktop) | Native app built on herdr-core. github.com/herdrdev/herdr-app, Apache-2.0, Rust plus Zig, 2.6k stars. Extended by JavaScript "mods" in `~/.config/herdr-app/mods/` | 0.1.15 | No. |
| Herdr mobile (iOS, Android) | Talks to herdr-core on each machine over SSH. "Herdr TUI isn't supported." No mods, no custom UI. | | No. |

The TUI and core do not share sessions. So anyone who wants the official phone app must use core, and loses the TUI's resume.

## Maintainer position

- herdr-core #13 "Restore agent sessions after a full stop or reboot": closed as not planned on 2026-10-27. Maintainer (ogulcancelik): core only supports agent features through OSC 7501; "when it does, we'll support it." No timeline.
- herdr-app PR #37 (session-reference capture): declined, because resume in the legacy TUI "got flaky" as agent CLIs kept changing how they own sessions.
- herdr-app #99: closed not planned, "Knowing limitation until OSC7501 has a `sessionId`."
- herdr-core #15 "Discuss carrying a resume reference in OSC 7501": open since 2026-10-29, about 10 user comments, no maintainer reply. **This is the one thing that could make herdr-wake redundant**, and the right place to announce it. herdr-wake's session-id source is a swappable part for that reason.
- Users expect resume because the TUI has it (herdr-app #406, herdr-core #138).
- Nobody has asked upstream for boot-time start.

herdr.dev FAQ: "If the machine reboots or herdr-core stops, the programs stop too... Herdr doesn't restart an agent or resume its work in progress."

## Existing community tools

About 30 projects do some form of agent resume for Herdr; 577 GitHub repos carry the `herdr-mod` topic, which feeds the gallery at herdr.dev/gallery automatically. Clones of 34 repos were read at source level.

| Project | Shape | Agents | Notable | Weakness |
|---|---|---|---|---|
| dmazlum/herdr-revive (9 stars, v0.5.1, 9 releases in 9 days, MIT) | Desktop-app mod in Node 20; hooks plus a block in `.zshrc`/`.bashrc` | Claude Code; Codex, Droid, Cursor, pi, omp experimental | `doctor`, backups, drops bypass flags on unattended resume (since v0.5.1), refuses world-writable folders, flag capture via `ps` works on macOS | No OpenCode; needs Node; macOS dropped from its CI; per-machine only |
| guillaume86/herdr-recover (0 stars, "0.2", MIT) | Shell scripts; installs itself as core's `shell`; works with no app | Claude Code, Codex, OpenCode 2, Pi | The only one that is headless by design; restores `CLAUDE_CONFIG_DIR`, `CODEX_HOME` | "A real reboot hasn't been tried yet"; nothing starts core after boot; Linux-only flag capture; carries bypass flags silently |
| thuanlm215/herdr-agent-resume (4 stars, no releases, MIT) | Python 3.8 mod; manual key or `--auto` rc block | Claude Code, Codex, Grok, Antigravity, Pi, Kiro | Claim lock, `adopt`, watcher that forgets a session when the agent exits and the shell lives | zsh path untested; no flags on macOS |
| cantona/herdr-revive | Snapshots, named spaces, crash-safe journal | | `preview`, `restore --dry-run` | Bash only; no flag restore |
| dougcunha/herdr-mods (agent-resume-app) | State as hc labels | | Forget and Resume buttons | Needs the app; guesses Codex sessions |
| manastalukdar/herdr-agent-resume-plus | Auto-resume only when the boot id changed | | Resumes sub-agents | Linux only; needs the app |
| Others (pine2D, quinnjr, seankao31, ninanoV, victor-software-house, terafin, nickmaglowsch, Aramillo for Windows, and more) | | | Single good ideas: lazy one-at-a-time resume (keychain race), re-check flags against `--help`, send "continue" only to agents that were working, Codex picker when ambiguous | |

## What users ask for that nobody had built

1. Starting herdr-core at boot with no app running (herdr-recover names this gap).
2. A real reboot test: almost every README says one was never run.
3. Detecting a Codex `/quit` (Codex reports the same end reason for a quit and a kill).
4. Recording Codex sessions while its daemon is on.
5. One tool covering Claude Code, Codex, Pi and OpenCode on macOS and Linux.
6. Open legacy-Herdr bugs that a new tool should not repeat: #5547 (stale resume after `/clear`), #5921 (OpenCode reopens as a new chat after a cold restart), #5837 (Codex auto-review thread takes over the pane), #5767 and #5879 (wrong folder).

## How herdr-wake is positioned

- **It starts core at boot.** No other project does.
- **It is one self-contained binary** with no Node, Python or Git Bash, and no dependency on the desktop app, so it behaves the same from the phone.
- **Four agents are first-class**: Claude Code, Codex, Pi, OpenCode.
- **It publishes dated reboot-test results** and labels anything untested as untested.
- **It restores exactly what was running**, bypass mode included, by design, stated plainly, with an opt-out.
- **It is a companion to core, not a workaround**: if core starts carrying a resume reference (#15), herdr-wake reads it from core and drops its hooks, and keeps the boot start, the quit-versus-kill rule, staggered starts, `status` and `doctor`.

## Sources

- https://herdr.dev/ , https://herdr.dev/core/ , https://herdr.dev/mobile/ , https://herdr.dev/gallery
- https://herdr.dev/tui/docs/session-state/ , https://herdr.dev/tui/docs/connecting-machines/
- https://herdr.dev/blog/connecting-the-machines/
- https://getmoshi.app/articles/herdr-core-vs-tui
- https://github.com/herdrdev/herdr-core (issues #13, #15, #138), https://github.com/herdrdev/herdr-app (issues #99, #406, PR #37), https://github.com/herdrdev/herdr/discussions/3228
- https://github.com/dmazlum/herdr-revive , https://github.com/guillaume86/herdr-recover , https://github.com/thuanlm215/herdr-agent-resume
- https://flaviocopes.com/herdr-phone/
