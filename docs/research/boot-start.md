# Boot start: getting herdr-core running before anyone connects

Researched 2026-11-10. herdr-core at `de97ea5`, paths under `crates/herdr-core/src/`. Nothing was run or installed.

## What core does today

- **Any client starts core** (`client.rs:85-123`, `194-218`); on Unix even `hc server status` does (`platform/unix.rs:44-50`, `cli.rs:551-562`). Tests and `doctor` must check with `pgrep`, never `hc`.
- **One server per state dir.** `serve` takes a lock on `core.lock`. A second `serve` that loses the race exits (`server.rs:43-56`). The socket path depends only on `HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR`, `HC_SESSION` and the config file (`paths.rs:17-48`). On macOS there is no `/run/user`, so the socket sits in `~/.local/state/herdr-core/default/` (`platform/unix.rs:130-136`).
- **macOS.** `serve` moves itself into a one-shot launchd job in **`gui/<uid>`** (`platform/macos_launchd.rs:119-146`, `177-255`, `438-463`). Over SSH it skips launchd and spawns directly with `setsid` (`macos_launchd.rs:148-166`, `platform/unix.rs:204-219`). If the bootstrap fails, it logs "launchd startup failed; starting directly" and keeps going (`macos_launchd.rs:169-175`).
- **Linux.** Core runs as a transient `systemd-run --user` unit, `hc-serve-<hex>`, with a shutdown-guard unit (`platform/systemd.rs:71-139`). With no user manager, it warns and spawns directly (`systemd.rs:56-66`). After a successful start it runs `loginctl --no-ask-password enable-linger` once, best effort (`systemd.rs:309-336`). Transient units do not survive a reboot, so nothing starts core at boot.
- **Saved environment does not cross a boot** (`restart.rs:12-13`, `README.md:33-34`). After a reboot, terminals inherit whatever started core.

### Will a later client find a core started from another launchd domain?

Yes, provided the boot job's `HOME` and `XDG_*` match the user's shells. If they match, the client connects to the same socket, and a racing second `serve` exits on the lock. If `XDG_STATE_HOME` or `HC_SESSION` differ, a **second core** starts. If only the socket path differs, the client gets "the core listens at X" (`client.rs:131-141`). The Herdr app replaces a core only when that core came from an *older GUI* login (`platform/session.rs:11-14`, `platform/macos_session.rs:50-55,82-86`). A core started before login is adopted as it is. Its terminals then live outside the GUI session (no clipboard, no `open`, no keychain prompts) until someone runs `hc server restart`. Prototype: does that restart keep terminals?

## 1. macOS

| | LaunchAgent, `gui/<uid>` | LaunchAgent, `user/<uid>` (`Background`) | LaunchDaemon + `UserName` |
|---|---|---|---|
| Fires | At GUI login, including auto-login | When the user domain is created: not at boot ("may exist independently of a logged-in user", launchctl(1)). Likely at the first SSH or GUI login; unverified | At boot, once the data volume is unlocked |
| SSH-only machine | No (unless auto-login) | At first SSH login, when core autostarts anyway | Yes |
| Login keychain | Unlocked (loginwindow unlocks it) | Locked | Locked (global security session, TN2083) |
| Home dir | Yes | Yes | Yes without FileVault; TCC folders (Documents, Desktop, Downloads) cannot prompt |
| FileVault | After unlock and login | Same | Fires after an SSH unlock (macOS 26+, Apple silicon, Remote Login on) or `fdesetup authrestart` |
| sudo to install | No | No | Yes (`/Library/LaunchDaemons`, root-owned) |

- **Keychain (the deciding fact).** Claude Code keeps macOS credentials in the login Keychain. When the Keychain is "locked in an SSH session", it falls back to `~/.claude/.credentials.json` ([Anthropic: Authentication](https://code.claude.com/docs/en/authentication)). Nothing unlocks the login keychain before a GUI login or auto-login. A pre-login Claude using Keychain credentials will most likely come up not authenticated. Pre-login resume only helps Claude if one of these holds:
  - the user's login already lives in the file fallback;
  - `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) is supplied (it ranks above `/login`);
  - an `apiKeyHelper` is set.

  Codex has the same question with `cli_auth_credentials_store = keyring|auto` ([OpenAI: auth](https://learn.chatgpt.com/docs/auth)).
- **Auto-login** is unavailable while FileVault is on ([Apple 102316](https://support.apple.com/102316)). Where it is on, the GUI agent gives full fidelity (keychain, GUI session) before any connection. The trade-off is physical-access exposure.
- **Background Task Management.** On macOS 13+, launchd items appear under Login Items with a notification when added; `sfltool dumpbtm` lists them ([Apple deployment guide](https://support.apple.com/guide/deployment/depdca572563/web)). The macOS 26 prompt targets apps whose tasks outlive them, not plists.
- **Daemon environment.** Set `HOME`, `USER`, `SHELL`, `PATH` explicitly; core falls back to `/` without `HOME` (`platform/unix.rs:117-120`). Its `gui/<uid>` bootstrap failure in `core.log` is expected.

**Recommendation.** Default: the GUI LaunchAgent, with no sudo. Opt-in (`install --before-login`): the daemon. `doctor` warns when Keychain credentials make the daemon pointless. The DECISIONS #10 wording, "resumes at login or on first remote connection", stays true for the default mode.

## 2. Linux

- **User unit plus linger.** With linger on, "a user manager is spawned for the user at boot and kept around after logouts" ([loginctl(1)](https://www.freedesktop.org/software/systemd/man/latest/loginctl.html)). Upstream polkit gives `set-self-linger` `allow_any=yes`, so no sudo is needed ([login1 policy](https://github.com/systemd/systemd/blob/main/src/login/org.freedesktop.login1.policy)). `set-user-linger` (for another user) needs admin. Distros can override this, so `doctor` checks. The unit runs `hc server start`, so core still becomes its own `systemd-run --user` unit with its shutdown guard. The two coexist; never run `serve` from our unit.
- **System unit with `User=`.** Needs root, has no `XDG_RUNTIME_DIR`. Without linger, core's socket lands in the state dir and later SSH logins fail with "listens elsewhere". Not supported.
- **Environment.**
  - User services get the manager's environment plus `environment.d` ([environment.d(5)](https://www.freedesktop.org/software/systemd/man/latest/environment.d.html)). `~/.local/bin`, nvm, bun and cargo paths are usually missing.
  - `XDG_RUNTIME_DIR` is set; there is no TTY.
  - With no desktop login, there is no session `DBUS_SESSION_BUS_ADDRESS` (core imports session variables only when the manager has them, `systemd.rs:15-30`).
  - Linux shells are non-login (`platform/unix.rs:254-258`), so `~/.profile` additions are lost unless they come from the core's environment.
- **Ordering.** `network-online.target` is a system-only target ([systemd.special(7)](https://www.freedesktop.org/software/systemd/man/latest/systemd.special.html)); `herdr-wake boot` waits for the network itself, bounded.
- **Restart.** Oneshot; the holder restarts a dead core (`holder.rs:440-462`).
- **No systemd** (cron `@reboot`, [crontab(5)](https://man7.org/linux/man-pages/man5/crontab.5.html); OpenRC): core has no user manager there either. Document only, not v1.
- **WSL2.** systemd needs `[boot] systemd=true` ([Microsoft: systemd](https://learn.microsoft.com/en-us/windows/wsl/systemd)), but "systemd services will NOT keep your WSL instance alive". Idle distros stop after `instanceIdleTimeout` (default 15 s; `-1` disables) ([wsl-config](https://learn.microsoft.com/en-us/windows/wsl/wsl-config)). Nothing starts WSL at Windows boot; "reboot" means a Windows restart or `wsl --shutdown`. A Windows logon task must run `wsl.exe -d <distro>`. Untested in v1.

## 3. Windows (coming soon)

Today core starts on demand only. It uses `DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB` (`platform/windows.rs:581-596`) and a per-user named pipe (`windows.rs:352-356`). It refuses an elevated start over SSH while a desktop user exists (`platform/windows_session.rs:13-27`).

| Option | Fires | Admin | Verdict |
|---|---|---|---|
| Task Scheduler, logon trigger, interactive token | At the user's logon | No | Use it ("User must already be logged on", [TASK_LOGON_TYPE](https://learn.microsoft.com/en-us/windows/win32/api/taskschd/ne-taskschd-task_logon_type)) |
| Boot trigger | When the scheduler service starts | Yes ([BootTrigger](https://learn.microsoft.com/en-us/windows/win32/taskschd/boottrigger)) | Only with a stored password. S4U has "no access to either the network or to encrypted files": useless for agents |
| Service | Boot, session 0 | Yes | No: conflicts with the desktop-pipe rule |

## 4. Reproducing the interactive environment

| Approach | Strength | Weakness |
|---|---|---|
| Captured at install | Simple | Goes stale (nvm bumps) |
| `$SHELL -lic` at launch | Always current | Runs rc files with no TTY: prompts, tmux auto-attach, slow frameworks, hangs |
| Captured per terminal at record time | Exact: the hook runs inside the agent's own process | Must never store secrets |

**Recommendation: a hybrid.**
- **Install time:** capture `PATH`, `LANG` and `LC_*` into a 0600 `boot.env` (`environment.d` on Linux, `EnvironmentVariables` on macOS). Core then has the environment a login would have given it.
- **Record time:** for each session, store the agent's **absolute binary path**, `PATH`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, and an allowlist (no `*_KEY` or `*_TOKEN`).
- **Launch:** only if the binary is gone, probe once with `$SHELL -lic 'command -v <agent>'` under a timeout.

## 5. Unit files

**macOS, default** (`~/Library/LaunchAgents/dev.heretik.herdr-wake.plist`):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>dev.heretik.herdr-wake</string>
  <key>ProgramArguments</key><array>
    <string>/Users/USER/.local/bin/herdr-wake</string><string>boot</string></array>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><false/>
  <key>AbandonProcessGroup</key><true/>
  <key>StandardOutPath</key><string>/Users/USER/Library/Logs/herdr-wake/boot.log</string>
  <key>StandardErrorPath</key><string>/Users/USER/Library/Logs/herdr-wake/boot.log</string>
</dict></plist>
```

Install `launchctl bootstrap gui/$(id -u) <plist>`, enable `launchctl enable gui/$(id -u)/dev.heretik.herdr-wake`, disable `launchctl disable gui/$(id -u)/dev.heretik.herdr-wake`, uninstall `launchctl bootout gui/$(id -u)/dev.heretik.herdr-wake` and move the plist to the Trash.

**macOS, before login** (`/Library/LaunchDaemons/dev.heretik.herdr-wake.USER.plist`, root:wheel 0644): the same as above with `Label` `dev.heretik.herdr-wake.USER`, without `LimitLoadToSessionType`, plus:

```xml
  <key>UserName</key><string>USER</string>
  <key>GroupName</key><string>staff</string>
  <key>EnvironmentVariables</key><dict>
    <key>HOME</key><string>/Users/USER</string>
    <key>USER</key><string>USER</string>
    <key>SHELL</key><string>/bin/zsh</string>
    <key>PATH</key><string>CAPTURED_AT_INSTALL</string></dict>
```

Install `sudo launchctl bootstrap system <plist>`. Disable `sudo launchctl disable system/dev.heretik.herdr-wake.USER`. Uninstall `sudo launchctl bootout system/dev.heretik.herdr-wake.USER`, then move the plist to the Trash.

**Linux** (`~/.config/systemd/user/herdr-wake.service`):

```ini
[Unit]
Description=Start herdr-core and resume agent sessions
[Service]
Type=oneshot
RemainAfterExit=yes
EnvironmentFile=-%h/.config/herdr-wake/boot.env
ExecStart=%h/.local/bin/herdr-wake boot
TimeoutStartSec=180
[Install]
WantedBy=default.target
```

- Install: `systemctl --user daemon-reload && systemctl --user enable herdr-wake.service && loginctl enable-linger`
- Disable: `systemctl --user disable herdr-wake.service`
- Uninstall: disable it, move the unit to the Trash, `daemon-reload`. Run `loginctl disable-linger` only if herdr-wake was the one that enabled linger (record that at install).

**`doctor` checks:**
- macOS: `fdesetup isactive`; `defaults read /Library/Preferences/com.apple.loginwindow autoLoginUser`; `sw_vers` 26+ and `arm64` (SSH unlock possible); `launchctl print gui/$UID/<label>` or `system/<label>`, plus `print-disabled`; `launchctl print gui/$UID` (is there a GUI login now?); whether Claude's credential is in the Keychain or in the file (never read its value).
- Linux: `/var/lib/systemd/linger/$USER` (core checks the same path, `systemd.rs:317`); `systemctl --user is-enabled herdr-wake`; `systemctl --user show-environment`; PID 1 is systemd; `WSL_DISTRO_NAME`.
- Both: the boot job's `HOME` and `XDG_*` match the current shell's.

## 6. Reboot test (per mode, nobody at the console)

1. Record versions, reboot. On FileVault plus 26+, SSH-unlock, disconnect.
2. After 3 minutes, SSH in. **Before any `hc` command** (it would start core):
   - **No other login:** `who`. On macOS `launchctl print gui/$UID` must fail; on Linux `loginctl list-sessions` shows only this SSH session.
   - **Started before you:** `pgrep -fl 'herdr-core serve'`; its `ps -o lstart=` is earlier than your SSH login (`last -1`).
   - **Agents up early:** agent processes (`pgrep -fl 'claude|codex|opencode|pi'`) also started before your login.
   - **Logs:** herdr-wake's log shows the boot run and summary.
3. Then `hc` read each resumed terminal: authenticated (the Keychain check) and in the right folder.
4. Repeat with the phone as the first connection.

## Recommendation

| OS | Mode | Fires when | sudo | Risks |
|---|---|---|---|---|
| macOS | GUI LaunchAgent (default) | GUI login or auto-login | No | SSH-only Macs without auto-login wait for the first connection |
| macOS | LaunchDaemon (opt-in) | Boot, after disk unlock | Yes | Keychain locked: Claude unauthenticated; TCC folders; core stays outside the GUI session |
| macOS | Auto-login + agent | Boot | Settings change | Physical access; not possible with FileVault |
| Linux | User unit + linger | Boot | Usually no | PATH gaps; distro polkit overrides |
| WSL2 | User unit + Windows logon task | Windows logon | No | Idle shutdown; untested |
| Windows | Logon task | User logon | No | Not before logon |

Also: launchd.plist(5), launchctl(1), fdesetup(8) (local man pages); [TN2083](https://developer.apple.com/library/archive/technotes/tn2083/_index.html); [macOS 26 notes, SSH FileVault unlock](https://support.apple.com/en-afri/124963).
