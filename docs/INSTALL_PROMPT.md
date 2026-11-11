# Install herdr-wake with your coding agent

Copy everything below the line and paste it into your coding agent (Claude Code, Codex, Pi, OpenCode or any other) on the machine where herdr-core runs. The agent checks your setup, explains what will change, asks before anything that needs `sudo` or touches your running sessions, installs herdr-wake and checks the result.

---

Install herdr-wake on this machine for me, following these steps in order. herdr-wake (https://github.com/Heretikio/herdr-wake) starts herdr-core at boot and resumes my coding-agent sessions in their herdr-core terminals after a reboot. Its README and docs/LIMITS.md are the reference if you need detail.

Ground rules for the whole task:

- **Never run herdr-wake itself with `sudo`.** It refuses to run as root. The only `sudo` commands in this task are ones herdr-wake prints for me (Step 3).
- **Ask first** before any command with `sudo`, and before anything that stops, restarts or reboots herdr-core or the machine. Wait for my yes. A stop or restart ends every program in every herdr-core terminal, possibly including you.
- **To check herdr-core, use only `hc --version`.** Other `hc` commands (even `hc server status`) start herdr-core as a side effect.
- **Run only these herdr-wake commands:** `install`, `doctor`, `status`, `adopt`, `uninstall` (only if I ask), and `--help`. `resume`, `forget`, `off` and `on` change what comes back after a reboot: run them only when I ask. `hook`, `watch`, `shell` and `boot` are for the system to call.
- **Explain in plain words.** When you report a check, say what it means for me and what to do, not just the raw line.

## Step 1: check prerequisites

1. Run `uname -sm`. macOS or Linux on arm64 (aarch64) or x86_64 can continue. On Windows, tell me herdr-wake for Windows is coming soon and stop.
2. Run `hc --version` (or `~/.local/bin/hc --version` if `hc` is not on `PATH`). herdr-core must be 0.1.16 or newer. If it is missing or older, tell me to install or update herdr-core (https://herdr.dev) and stop.
3. Check which agents I have: `command -v claude codex pi opencode`, and the version of each one found. Pi needs 0.79.4 or newer; for OpenCode 2.x the minimum is 2.0.28. herdr-wake sets up only the agents it finds (a config folder that exists, or the program on `PATH`), so an agent that is missing here will be skipped.
4. Check whether you are running inside a herdr-core terminal: `HC_TERMINAL_ID` is set in your environment if so. Remember the answer for Step 6.

Done when you can tell me: OS and CPU, herdr-core version, which agents I use with their versions, and anything that blocks the install.

## Step 2: explain what install does, and get my go

Tell me these five things in your own words, then ask my choices:

1. After a reboot, herdr-wake restarts my agents with nobody watching, with the same flags they had, **including permission-bypass flags** such as `--dangerously-skip-permissions` or `--yolo`. Ask whether I want bypass flags dropped instead (`restore_bypass = false`). Tell me that this drops launch flags only (the list is in the README) and does not change what my agents' own config files allow.
2. It sets herdr-core's `shell` setting to herdr-wake, so every recovered terminal starts through it. If the herdr-wake binary is ever deleted without `herdr-wake uninstall`, those terminals cannot start until the setting is fixed.
3. On macOS only: by default, with auto-login off, nothing runs after a reboot until the first connection (for example the Herdr desktop app on another machine, or SSH running `hc`) starts herdr-core; my sessions then come back within seconds. That case has been tested. To have them running before any connection, I can turn on auto-login or use `--before-login` (which starts herdr-core at boot but needs `sudo` steps); neither has been reboot-tested yet. Until someone logs in at the Mac's screen the keychain is locked: Claude Code stayed signed in when its credentials were in `~/.claude/.credentials.json`, and may come up signed out when they are only in the keychain. Ask whether I want `--before-login`. On Linux, skip this: the user service starts herdr-core at boot with linger, with nobody logged in.
4. A resumed agent loads its folder's hooks and MCP servers again with nobody watching, so herdr-wake only records and resumes sessions in folders that are mine alone (owned by me, not writable by others or by a shared group, not under a folder someone else controls). Sessions elsewhere get a plain shell after a reboot.
5. What real reboots showed: Claude Code asks whether to trust a folder it has not been told to trust (every time for the home folder), and after a reboot nobody is there to answer, so I should keep agent sessions in project folders. Mode changes I make inside an agent (for example switching Claude Code to auto mode with shift+tab) are not restored; the launch flags are. Terminals created with `hc run` come back but are not shown in the Herdr desktop app (the phone app lists them).

Done when I have answered both questions (bypass flags; before-login on macOS) and said to go ahead.

## Step 3: install

1. Run, from a shell where my agents are on `PATH` (install saves that `PATH` for boot):

   ```sh
   curl -fsSL https://github.com/Heretikio/herdr-wake/releases/latest/download/install.sh | sh
   ```

   For `--before-login`, end the line with `| sh -s -- --before-login` instead.
2. Show me the output. Lines like `skipped codex: not installed` are expected for agents I do not have; if I install one later, `herdr-wake install` again picks it up. If the output has a section headed `NOT FINISHED. RUN THESE YOURSELF`, those are `sudo` commands: show them to me and ask. Run them only after my yes, or let me run them myself. The first one (`sudo plutil -lint ...`) checks the file that is about to be installed; show me that file if I want to read it.
3. If the installer warned that `~/.local/bin` is not on my `PATH`, tell me how to add it.
4. If I chose to drop bypass flags, add `restore_bypass = false` as a new line in `~/.config/herdr-wake/config.toml`, keeping the lines already there (install wrote `real_shell` into it).

Done when the installer finished without an error and any `sudo` steps are done or handed to me.

## Step 4: run the doctor

Run `herdr-wake doctor`. Each line is `ok`, `warn` or `FAIL`, and many have a `fix:` line under them. The command exits 1 when any check fails.

- Explain every `FAIL` and every `warn` to me in plain words: what it means for my sessions after a reboot, and the fix.
- Apply a fix yourself only when it changes my own files and I said yes. Anything with `sudo`: ask first.
- Expected and harmless: a `codex trust` warning appears whenever Codex is set up. It matters only if I use Codex (Step 5).
- A `FAIL  config:` line means `~/.config/herdr-wake/config.toml` has a typo. Until it is fixed nothing is resumed and nothing is recorded: fix it first.
- With `--before-login` on macOS, a `wake time` warning about Claude Code and the keychain is expected; repeat its advice to me.
- Read me the `wake time` line: it says when my agents will come back on this machine.

Done when `herdr-wake doctor` shows no `FAIL`, or every remaining `FAIL` has been explained to me with its fix, and each `warn` has been explained.

## Step 5: Codex approval (only if I use Codex)

Codex runs a new hook only after it is approved once. Tell me to open Codex in a Herdr terminal, type `/hooks`, and trust the herdr-wake hook. If you are Codex yourself, tell me to type `/hooks` here. herdr-wake cannot see this approval, so the doctor reminder stays.

Done when I confirm I approved it, or I tell you I do not use Codex.

## Step 6: record sessions that are already running

1. Run `herdr-wake adopt`. It records Claude Code sessions that were running before the install. Tell me which terminals it recorded.
2. For Codex, Pi and OpenCode sessions it lists as needing a restart: tell me to quit and restart each one once in its terminal, so its hook or plugin records it.
3. Run `herdr-wake status` and show me the list. If a session shows as `untrusted`, tell me: herdr-wake will not resume it until I accept it with `herdr-wake resume <terminal>` (after reading the command it prints) or it is recorded afresh. If one I expected is missing, look at the end of `~/.local/state/herdr-wake/herdr-wake.log`: a line with `is not safe to resume in` names a folder whose permissions I should tighten (`chmod go-w`), and `not restorable` names a flag whose value was left out because it could hold a secret.

Done when every agent I have running in a Herdr terminal shows in `herdr-wake status` as `running`, or I know which ones I still need to restart.

## Step 7: verify safely

1. Ask me to start a new agent session in a Herdr terminal (any agent I use), then run `herdr-wake status` again and confirm the new session is listed as `running`. This shows recording works without touching anything that is running.
2. Tell me that the real proof is a reboot, or stopping and starting herdr-core, and that both stop every program in every herdr-core terminal. Leave the choice and the timing to me; offer it, do not do it.
3. If I want to test now and say yes:
   - If you are inside a herdr-core terminal (Step 1, item 4), stopping core ends your own session: give me the commands to run myself, `hc server stop` and then `hc server start`.
   - Otherwise you may run them, after my yes.
   - Afterwards, `herdr-wake status` and opening the terminals should show the sessions back. If a terminal shows a line starting `herdr-wake: could not resume`, read it to me: it names the reason and the exact command to run.

Done when I have seen a new session recorded in `herdr-wake status`, and I have either run the restart test or chosen to skip it.

## Finish

Give me a short summary: the herdr-wake and herdr-core versions, the doctor result, when my agents will come back after a reboot (the `wake time` line), whether bypass flags are restored, and how to undo everything: `herdr-wake uninstall`, or `curl -fsSL https://github.com/Heretikio/herdr-wake/releases/latest/download/uninstall.sh | sh`. Mention that herdr-wake's real reboot tests so far cover Claude Code only (on Linux x86_64 and on a Mac at its login screen; see the README's test table), and that Codex, Pi and OpenCode have not been reboot-tested with the real agents, so I should check my sessions after my first real reboot.
