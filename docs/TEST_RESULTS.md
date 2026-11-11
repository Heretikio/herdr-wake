# Test results

Dated record of what has actually been run. Newest first. Anything not listed here has not been tested.

## 2026-11-10: macOS reboot with the phone app connecting first (owner-reported)

**Machine and versions:** the same Apple silicon Mac as below (macOS 27.2, auto-login off, herdr-core 0.1.18, Claude Code 2.1.322), herdr-wake 0.1.0 at commit a4da220.

The owner rebooted the Mac, did not log in at its screen, and opened the Herdr phone app first. The owner reported that it "seemed to work". Checked afterwards over SSH:

| Time | Event |
|---|---|
| 20:59:38 | first client connection (`herdr-core connect`), which started herdr-core at 20:59:39 |
| 20:59:39 | a second client connected, from a different address |
| 20:59:39 | `claude --resume` started for the session created in the desktop app |
| 20:59:41 | `claude --resume` started for the command-line test session |

Both real Claude Code sessions were running and signed in three minutes later, with no desktop login. **Caveat:** two clients connected within one second of each other, so the logs alone cannot show that the phone was the one that started herdr-core; that part rests on the owner's report.

## 2026-11-10: first real reboot on macOS, real Claude Code, app as first connection

**Machine:** Mac, Apple silicon, macOS 27.2 (Darwin 27.2.0), FileVault off, auto-login off. herdr-core 0.1.18, Claude Code 2.1.322 (credentials in `~/.claude/.credentials.json`).
**Version:** herdr-wake 0.1.0 at commit a4da220.
**Setup:** `herdr-wake install` over SSH while the Mac sat at its login screen (no desktop session). The LaunchAgent could not be loaded ("loads at the next login"), which is the expected state for this Mac. Two real Claude Code sessions in kept herdr-core terminals:

| Terminal | Created by | Folder |
|---|---|---|
| t5 | `hc run --keep` over SSH, then `claude` typed in | a project folder, one exchange in the conversation |
| t4 | the owner, from the Herdr desktop app on another Mac | the home folder |

**Action:** the owner rebooted the Mac and did not log in at its screen. The first connection afterwards was the Herdr desktop app on another machine.

**Result: pass.**

| Time | Event |
|---|---|
| 20:53:56 | herdr-core started, by the app's connection (no boot service on this Mac yet) |
| 20:53:58 | `claude --resume <t5 session>` started by herdr-wake |
| 20:54:00 | `claude --resume <t4 session>` started by herdr-wake |

- Both real sessions came back signed in, with nobody logged in at the desktop. The t5 terminal showed its earlier exchange. The owner confirmed their own session "resumed properly".
- No trust prompt in the project folder.
- At shutdown the two SessionEnd hooks ran after herdr-core had stopped and logged "core is not running"; the records stayed, which is the wanted outcome.

**Finding: terminals made with `hc run` are not shown in the Herdr desktop app, but are shown in the phone app.** The desktop app lists terminals that carry its own `herdr.space` and `herdr.tab` labels. A terminal created from the command line has neither, so it resumes but the desktop app does not draw it. The owner confirmed the same terminal is visible in the Herdr phone app. herdr-wake resumes both kinds.

## 2026-11-10: second real reboot, Linux x86_64, real Claude Code plus stand-ins

**Machine:** the same Fedora Linux 44 machine (x86_64, bare metal, systemd, lingering on), herdr-core 0.1.18.
**Versions:** herdr-wake 0.1.0 at commit aa278c8 (with the security fixes and the monotonic clock), Claude Code 2.1.304.
**Upgrade path exercised:** the earlier build was uninstalled (config files restored, boot unit removed), the new build installed. `install` skipped Codex, Pi and OpenCode ("not installed") and created nothing for them. Records left by the earlier build showed as `untrusted` and were not resumable until accepted with `herdr-wake resume <terminal>`.

| Terminal | Before reboot | After reboot |
|---|---|---|
| t7 | **real Claude Code**, signed in, one exchange in the conversation, started by hand in the home folder | `claude --resume <same id>` was started by herdr-wake 25 s after boot. It stopped at Claude Code's "do you trust this folder?" prompt. After answering the prompt the earlier conversation was on screen. |
| t8 | stand-in, `--model opus` | resumed, same folder and session |
| t9 | stand-in, `--dangerously-skip-permissions --model sonnet`, folder with a space | resumed with both flags |
| t12 | stand-in, `--model haiku` | resumed |
| t10 | stand-in, quit on purpose | plain shell |
| (hw4) | plain shell | plain shell |

**Result: pass, with one finding.**

- herdr-core was started by the boot service 20 s after boot, with nobody logged in, before any client connected.
- Start times: 20:28:32, :34, :35, :37. The first agent started alone and the rest followed 1 to 2 s apart, although the system clock was again stepped back about 130 s during boot. The monotonic clock fix works.
- All four watchers logged "left record: shutting down" at the reboot and wrote nothing.
- Boot summary: `3 resumed, 0 failed, 1 unconfirmed, 1 skipped`. The unconfirmed one was the real Claude Code session, reported accurately: the agent had not run its hook within 30 s, because it was waiting at the trust prompt.
- First time the real Claude Code hook recorded a session, and first time the real `claude --resume` was run by herdr-wake after a real reboot.

**Finding: Claude Code's folder trust prompt blocks an unattended resume.** In a folder Claude Code has not been told to trust (and it asks every time for the home folder), the resumed agent waits at the prompt until a person answers it. herdr-wake reports the terminal as unconfirmed. Sessions in project folders that were trusted once are not expected to hit this; not yet verified.

**Also seen:** the session had been switched to auto mode inside Claude Code (shift+tab) before the reboot. herdr-wake records the flags an agent was started with, not mode changes made later, so that is not restored.

## 2026-11-10: first real reboot, Linux x86_64, stand-in agent

**Machine:** Fedora Linux 44 Workstation, kernel 6.19.12, x86_64, bare metal, systemd, user lingering on.
**Versions:** herdr-wake 0.1.0 (commit a6b4e25, before the security fixes), herdr-core 0.1.18.
**Agent:** the test stand-in (`tests/stubs/agent.sh`) launched under the name `claude`, calling the real `herdr-wake hook claude start` and `end`. Real Claude Code was not used: it is not signed in on this machine.

**Setup:** `herdr-wake install` for real (Claude hook, core `shell` setting, systemd user unit). Four kept herdr-core terminals:

| Terminal | Folder | State before reboot |
|---|---|---|
| t3 | `proj1` | agent running, `--model opus` |
| t4 | `proj with space` | agent running, `--dangerously-skip-permissions --model sonnet` |
| t5 | `proj3` | agent quit on purpose (end hook fired, record "ended") |
| t6 | `proj2` | plain shell, never had an agent |

**Action:** `sudo systemctl reboot`. Nobody logged in at the console (the display manager's greeter was showing). First check was over SSH, 2 minutes 8 seconds after boot, and ran no `hc` command.

**Result: pass.**

| Check | Observed |
|---|---|
| herdr-core started by the boot service, not by a client | Booted 19:11:11, `herdr-core serve` started 19:11:30. Boot summary: "started core: hc server start". |
| Running agents came back | t3 resumed at 19:11:32 with `--model opus --resume <same id>`, t4 at 19:11:40 with `--dangerously-skip-permissions --model sonnet --resume <same id>`. Same folders, same session ids, `CLAUDE_CONFIG_DIR` restored. |
| The quit agent stayed gone | t5 came back as a plain shell in `proj3`. |
| The plain shell stayed a plain shell | t6 came back as a plain shell in `proj2`. |
| Shutdown was not mistaken for a quit | Both watchers logged "left record: shutting down" and wrote nothing. |
| Boot summary | `2 resumed, 0 failed, 1 skipped`. |
| Quitting a resumed agent | The same terminal became `/bin/bash` in the session's folder and stayed usable; the record changed to "ended". |

**Problems found:**

1. **Clock step during boot.** The hardware clock was 130 s ahead; chronyd stepped the system clock back after herdr-wake had started. The boot summary's finish time is earlier than its start time, and the second agent waited the full 8 s first-alone hold although the first had confirmed. Cause: gate timing uses wall-clock time. Fix in progress: use a monotonic clock for everything measured within one boot.
2. **Install set up agents that are not installed.** It created `~/.codex`, `~/.pi/agent` and `~/.config/opencode` on a machine with only Claude Code. Fix in progress: skip absent agents.
3. **herdr-core labels the terminals' agent as "herdr-wake"** in `hc ls`, because the terminal's program is the wrapper. Cosmetic; to be checked in the Herdr app.

## 2026-11-10: automated suites

| Platform | What ran | Result |
|---|---|---|
| macOS arm64 (Darwin 27.2), herdr-core 0.1.20 | 593 tests at the latest build (460 at the first run), including 23 end-to-end tests that drive a real herdr-core in an isolated home with stand-in agents (stop and start of core in place of a reboot) | all pass |
| Linux arm64 (Debian bookworm container), herdr-core 0.1.20 | 595 tests at the latest build (466 at the first run), including the same 23 end-to-end tests and the Linux-only platform tests | all pass |
| Linux x86_64 | release build; `doctor` and `status` on Fedora 44 | run by hand, behaved correctly |

The end-to-end and process tests do not run reliably under CPU emulation (19 failures when an x86_64 image ran on an arm64 host). Run them on native hardware.

## Not yet tested

- Real Codex, Pi or OpenCode being resumed by herdr-wake (stand-ins only so far). Real Claude Code has been resumed on Linux x86_64 and on macOS arm64.
- Real Claude Code in a trusted project folder (the one real test hit the trust prompt in the home folder).
- macOS with a desktop login or auto-login (the LaunchAgent path). The one macOS test so far was the login-screen case, where the first remote connection starts core.
- The phone app as the only client after a reboot (it was reported as first once, with another client connecting a second later).
- `--before-login` on macOS.
- Linux arm64 real reboot.
- The install script against a real release.
