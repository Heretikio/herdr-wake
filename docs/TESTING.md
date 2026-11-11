# Testing herdr-wake

There are three levels of testing. Only the last one earns the word "supported".

1. **Unit tests** drive each module through its interface with fakes: a fake herdr-core (`FakeCore`), a fake platform, and hook payloads captured from the real agents.
2. **End-to-end tests** run the built `herdr-wake` binary against a real herdr-core in a throwaway home folder, with a shell script standing in for each agent. Stopping and starting that core stands in for a reboot.
3. **The real reboot matrix** reboots real machines with real agents. See [below](#the-real-reboot-matrix).

Status on 2026-11-10: levels 1 and 2 pass on macOS arm64 (593 tests) and Linux arm64 (595 tests), 23 of them end-to-end against herdr-core 0.1.20. Level 3 has been run in part, for Claude Code only: two real reboots on a Fedora 44 x86_64 machine and one on an Apple silicon Mac. No test of the matrix below has been completed as written. Every run is recorded in [TEST_RESULTS.md](TEST_RESULTS.md).

## Running the suites

You need a stable Rust toolchain (`rustup`). For the end-to-end tests you also need herdr-core's `hc` (see "Which `hc` the tests use" below); without it those tests print a notice and pass without running.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo test` runs everything: unit tests in `src/`, the per-module tests in `tests/*.rs`, and the end-to-end suite in `tests/integration/`.

Run only the end-to-end suite:

```sh
cargo test --test integration
```

Skip it (unit tests only), by pointing the harness at an `hc` that does not exist:

```sh
HERDR_WAKE_TEST_HC=/nonexistent cargo test
```

Lint the Linux code from a Mac (the Linux platform adapter only compiles there):

```sh
rustup target add x86_64-unknown-linux-gnu
cargo clippy --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy and the full test suite on macOS arm64 and Ubuntu x86_64, after installing the newest herdr-core with its official installer so the end-to-end tests run. A separate lint job fails on any em dash character and runs shellcheck on `scripts/`. The nightly job runs the full suite against the newest herdr-core and opens an issue when it fails.

### Which `hc` the tests use

In order: `HERDR_WAKE_TEST_HC`, then `~/.local/bin/hc`, then the first `hc` on `PATH`.

## How the end-to-end harness keeps your real sessions safe

The end-to-end tests start real herdr-core servers on your machine, so they are built to never touch the one you use. The rules live in `tests/integration/harness.rs`:

- **A throwaway home per test.** Each test gets its own `HOME` in a temporary folder, so its herdr-core has its own state, config, labels and socket.
- **A short socket path.** A Unix socket path is limited to about 103 bytes and a temp home is deeper than that, so each test's core config sets `socket = "/private/tmp/hw-<pid>-<n>/c.sock"` (macOS) or `/tmp/hw-<pid>-<n>/c.sock` (Linux).
- **A cleared environment.** Every `hc` call runs with an empty environment plus that `HOME`, a minimal `PATH`, `SSH_CONNECTION=test` and `XDG_RUNTIME_DIR` set to the test's own socket folder. No `HC_*` or other `XDG_*` variable is passed, so nothing can point at your real core. The private `XDG_RUNTIME_DIR` matters on Linux: without it core falls back to `/run/user/<uid>` and, where a systemd user manager runs, starts the test core as a real user service.
- **No launchd.** On macOS, herdr-core's normal start path moves itself into a launchd job and cleans up stale `dev.herdr.core.*` jobs, which are shared with your real core. `SSH_CONNECTION` makes core take its direct start path instead.
- **No boot service.** The tests run `herdr-wake install` with `HERDR_WAKE_SKIP_BOOT_SERVICE=1`, so it writes no plist or unit and calls neither launchctl nor systemctl. This switch is for tests only.
- **Clean up and check.** Dropping a test's harness stops its core, waits until every process with that `HOME` is gone, and checks that every herdr process that was running before the suite started is still there.
- **Stand-in agents.** `tests/stubs/agent.sh` plays Claude Code, Codex, Pi and OpenCode: it picks or resumes a session id, creates the session file the adapter checks for, runs `herdr-wake hook <agent> start` with a realistic payload, then waits. Files under `$HOME/stub` make it quit like a person, fail on resume, or skip its hook.
- **A reboot** in these tests is `hc server stop` followed by `hc server start`. Kept terminals come back running the configured `shell`, which is what a reboot does to them. What it does not reproduce is listed in [LIMITS.md](LIMITS.md#what-has-not-been-tested).

If you want extra certainty, compare `pgrep -fl herdr` before and after `cargo test`.

## Linux tests in Docker

On a Mac, Docker runs Linux arm64 containers by default. This runs the unit tests there, with the source mounted read-only and the build kept inside the container:

```sh
docker run --rm -v "$PWD":/src:ro -w /src -e CARGO_TARGET_DIR=/tmp/target \
  rust:1 cargo test --locked
```

The image has no herdr-core, so the end-to-end suite skips. Install herdr-core in the container first (`curl -fsSL https://releases.herdr.dev/core/install.sh | sh`, as CI does on Ubuntu) to run it too. The Linux arm64 results in [TEST_RESULTS.md](TEST_RESULTS.md) came from a Debian container with herdr-core 0.1.20 in it.

`--platform linux/amd64` runs x86_64 under emulation, which is slow, and the end-to-end and process tests do not run reliably there (19 failures when an x86_64 image ran on an arm64 host). Run those on native hardware.

## The real reboot matrix

This is the test that earns "supported" (decision 13 in [DECISIONS.md](DECISIONS.md)). The matrix is ten tests:

- each of the four agents (Claude Code, Codex, Pi, OpenCode) on macOS and on Linux, with SSH as the first connection after boot (8 tests);
- one test per OS where the Herdr phone app is the first connection after boot (2 tests).

Each test uses real hardware, the real agent, and nobody at the console. Each run is written up in [TEST_RESULTS.md](TEST_RESULTS.md), and its row in the README table gets the date, versions and a one-phrase result. Anything not run stays "not yet run"; an untested combination is never called supported. A run that covers only part of a test (for example a different first connection) gets its own row in the README, saying what it covered.

On a Mac with auto-login off, a tested variant is the login-screen case: the first connection after the reboot (for example the Herdr desktop app on another machine) starts herdr-core, and the check is how soon after that connection the agents are back.

### Before the reboot

1. Record the versions: `herdr-wake --version`, `hc --version`, the agent's `--version`, and the OS (`sw_vers` on macOS, `cat /etc/os-release` on Linux), plus the CPU (`uname -m`).
2. Install herdr-wake and run `herdr-wake doctor`. Fix every `FAIL`. Note the `wake time` line: it says which case this machine is in (at boot, at login, before login with the daemon, after a FileVault unlock). Note FileVault and auto-login on a Mac, linger on Linux.
3. For Codex, approve the herdr-wake hook in Codex's `/hooks` first.
4. In Herdr, set up these terminals:
   - **A**: the agent under test, started by hand in a known folder with at least one restorable flag (for example `--model`) and, if you are testing bypass restore, a bypass flag. Send it a message with a unique word in it.
   - **B**: a second session of the same agent in another folder, with its own unique word.
   - **C**: the same agent, started and then quit normally before the reboot. It must not come back.
   - **D**: a plain shell with no agent. It must come back as a plain shell.
5. Run `herdr-wake status`. A and B must show as `running`; C as `ended` (or with no record).

### The reboot

6. Reboot without anyone at the console, and note how: the Apple menu's Restart, `sudo shutdown -r now`, `systemctl reboot`. (They differ: on macOS only a restart through the login window is seen by the shutdown detection.)
7. Mac with FileVault on macOS 26 or newer: unlock over SSH, then disconnect.

### After the reboot (SSH-first tests)

8. Wait 3 minutes. Then SSH in. **Run no `hc` command yet: any `hc` command starts herdr-core and spoils the test.**
9. Check, and copy the output into the notes:
   - **No other login:** `who`. On macOS, `launchctl print gui/$(id -u)` must fail (no desktop session) unless auto-login is on. On Linux, `loginctl list-sessions` shows only this SSH session.
   - **Core started before you** (boot-time modes): `pgrep -fl 'herdr-core serve'`, then `ps -o lstart= -p <pid>`, earlier than your login in `last -1`. In the macOS default mode without auto-login, core is expected to be not running yet: note that, then run `hc` once to make it start, and continue.
   - **Agents up early:** `pgrep -fl 'claude|codex|opencode|pi'`, started before your login in boot-time modes.
   - **herdr-wake's own account:** `cat ~/.local/state/herdr-wake/last-boot.json` and the end of `~/.local/state/herdr-wake/herdr-wake.log`; the boot job's log (`~/Library/Logs/herdr-wake/boot.log` for the LaunchAgent, `journalctl --user -u herdr-wake` on Linux; a `--before-login` LaunchDaemon has no log file of its own, its run is in `herdr-wake.log`).
10. Now run `herdr-wake status`, then open each terminal (Herdr app or `hc`) and check:
   - **A and B**: the same conversation (the unique word is there), in the right folder, with the expected flags (`ps -o args= -p <pid>`), and signed in (send a message and get an answer).
   - **C**: not resumed.
   - **D**: a plain shell in its folder.
   - Quit A: your normal shell appears in the same terminal, in A's folder.
   - `herdr-wake doctor` has no `FAIL`.

### After the reboot (phone-first tests)

Same as above, except the first connection after the reboot is the Herdr phone app: no SSH, no desktop login, no `hc` before it. Open the terminals from the phone and make the checks in step 10 there. Afterwards, SSH in to collect step 9's output, noting that the phone connected first.

### Pass or fail

A test passes when every check in step 10 holds without anyone typing a resume command, and the timing matches the mode `doctor` reported. Anything else is a fail; write down what was seen.

### Results template

Fill in one block per test and add it to [TEST_RESULTS.md](TEST_RESULTS.md), newest first, with what was seen (times from `herdr-wake.log`, the boot summary, anything unexpected). Then fill in or add the row in the README's table.

```
Test #:                 (1 to 10)
Date:
Host OS and version:
CPU:
Boot mode (doctor "wake time"):
FileVault / auto-login / linger:
How rebooted:
First connection after boot:     SSH | phone app | desktop app on another machine | none (core started at boot)
Agent and version:
herdr-core version:
herdr-wake version:
Core started before first login: yes | no | n/a (login-time mode)
A resumed (same conversation, folder, flags, signed in):  yes | no
B resumed:                        yes | no
C stayed closed:                  yes | no
D plain shell:                    yes | no
Shell after quitting A, right folder: yes | no
last-boot.json counts:
Result:                 pass | fail
Notes:
```

README row:

```
| <#> | <OS> | <agent> | <first connection> | <pass or fail, in one phrase> | YYYY-MM-DD | herdr-wake x.y.z, herdr-core x.y.z, <agent> x.y.z |
```
