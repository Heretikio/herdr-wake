# Prototype findings: the `shell` wrapper loop

Run 2026-11-10 on an Apple silicon Mac against herdr-core 0.1.20 (`~/.local/bin/hc`). The throwaway prototype scripts are not kept in this repository; A and B ran by hand (logs in the scratch home `proto-home/hA`). Every core ran with `env -i HOME=<scratch>/proto-home/<name> PATH=/usr/bin:/bin:/usr/sbin:/sbin SSH_CONNECTION=...`, no HC_* or XDG_*. No real-home core was running (no socket), so the read-only probe was skipped.

## A. `shell` wrapper launch: confirmed

`hc run --bg --keep` (no argv) started the wrapper in the caller's folder, parent `herdr-core hold`. Its full env:

```
COLORTERM=truecolor  HC_SCOPE=<token>  HC_TERMINAL_ID=t3  HC_TERMINAL_SOCKET=/private/tmp/hwp-hA/c.sock
HOME=<proto-home>/hA  LANG=en_US.UTF-8  PATH=/usr/bin:/bin:/usr/sbin:/sbin  PWD=<proto-home>/hA/work/a
TERM=xterm-256color  TERM_PROGRAM=ghostty  __CF_USER_TEXT_ENCODING=0x1F5:0x0:0x0
```

No `SHELL`, `USER`, `LOGNAME`, `TMPDIR`, `SSH_*`. argv is exactly the configured array (no `-l`).

## B. Labels and recovery: confirmed

A label written from inside t1 by raw `terminal.label`, then `cd ../b`, then `hc server stop` (84 ms). Records stay `live`, `reason=server_stop`, `last_cwd=work/b`, labels intact. Then `hc serve` with no client at all:

```
serve launched   1794354275.682
1794354275.872 t1 start cwd=.../hA/work/b
1794354275.872 t1 terminal.get ... reason=None labels=['note']
first client     1794354277.753
```

Recovery is eager (+0.19 s, before any client) and uses the last folder. `reason` was already null at the wrapper's first call in every run, so labels are the only recovery signal. "Lost" reasons: `holder_lost`, `stopped_by_restart`, `server_stop`, `host_shutdown`, `signaled`; an exit code or a kill is final. `hc server stop` is a fair stand-in for terminal state. A reboot adds: the holder dies too (`stopped_by_restart`), a new boot id, core started by the boot job's env, no GUI session or keychain, and the OS killing processes in its own order (see H5).

## C. Full loop with a stub agent: confirmed (after one fix)

| Stage | t1 `hc get` | Record |
|---|---|---|
| plain shell | fg `zsh -l`, status null | none |
| stub started by hand | fg `stub --session S1 --dangerously-...`, status `{app:stub,state:idle}` | live, seq 1 |
| stop | | live; watcher: "shell died with agent: record kept" |
| start (+0.27 s) | fg `stub --resume S1 --dangerously-...`, status idle | live, seq 2, resumed |
| `/exit` in agent | fg `zsh -l`, status null, same pid 21341 | ended by wrapper |
| `echo ALIVE pid=$$` | `ALIVE pid=21341` | |
| stop and start again | wrapper: "no live record (ended)", exec zsh | |

t2, quit before the stop: the watcher ended it 2.7 s later; it came back a plain shell.

## D. Missing shell binary: confirmed (core retries forever, silently)

```
+ 0.2s holder.log: holder: t1: cannot start "/nonexistent/herdr-wake": No such file or directory
+ 6.3s holder.log: (same)
+18.2s holder.log: (same)
+25.1s config restored (no restart)
+48.3s state=live reason=None pid=22247   wrapper starts, exec zsh -l
```

The API showed `live / server_stop / pid null` throughout; `core.log` empty, `holder.log` untimestamped. A fix applies at the next backoff slot (up to 60 s).

## E. Stagger: confirmed

Six kept terminals with live records, each resumed stub taking 2 s to fire its hook:

```
+0.222..0.229 six wrappers start      +0.223 t1 gate passed first=True
+2.306 t1 gate confirmed (record re-stamped by the resumed agent)
stub starts: +0.258 t1, +3.867 t5, +5.360 t4 (+1.493), +6.868 t6 (+1.508), +8.366 t2 (+1.498), +9.924 t3 (+1.558)
```

Gate = `flock` plus `gate.json {first, confirmed, last}`, 10 s confirm timeout. Order is lock-race order.

## F. Socket timing: confirmed, cheap

```
idle      n=1000 connect_p50=0.006ms total_p50=0.047ms p99=0.155ms max=1.697ms
6 parallel n=500 each: total_p50=0.134ms p99<2.0ms max=4.4ms
python3 start + 1 call p50=40.5ms   hc get p50=8.3ms   connect, no core: refused in 0.015ms
```

A 2 s budget is ~1000x the worst call; it is spent only on a hung core (H4).

## G. Explicit argv: confirmed

`hc run --keep -- zsh`, `-- /bin/sleep 600` and a shell line never launched the wrapper. After stop/start all four kept terminals came back with `argv=[wrapper.sh]`, fg `zsh -l`, the shell-line one in its last folder (`work/b`). 

## H. Surprises

1. **`foreground` names the process-group leader.** An agent run as a plain child showed the *wrapper* as foreground. With `setpgid` + `tcsetpgrp` (ignore SIGTTOU, take the tty back before exec), fg showed the agent. Matters for Codex binding and core's status clear.
2. **Stale write-back.** My first wrapper ended the record from its pre-resume copy, overwriting the agent's seq 2 with seq 1 and old pids. Re-read and compare `agent_pid` before ending.
3. **SIG_IGN survives exec.** The wrapper must restore SIGINT, SIGQUIT and SIGTTOU before exec'ing the shell.
4. **A hung core accepts connections.** With core SIGSTOPped, `connect` succeeded and the read timed out: fallback to zsh at +2.078 s. The deadline must cover the read.
5. **Shutdown ordering can fake a quit.** Interactive zsh ignores SIGTERM. SIGTERM to agent and shell only: zsh lived on and the watcher marked the record **ended**. SIGTERM to the watcher too: record kept. Core itself treats exits during a macOS shutdown as `host_shutdown` (loginwindow kills first); the watcher has no such signal.
6. **Socket path limit.** A scratch HOME this deep gives a 159-byte holder socket path (limit 103), so the harness needs `socket = "/private/tmp/hwp-<n>/c.sock"` in the isolated config.
7. **The launchd start path is shared across homes.** It boots out stale `dev.herdr.core.*` jobs in `gui/<uid>` and prunes `/tmp/hc-launchd-*`. `SSH_CONNECTION` forces the direct path; tests need that.
8. A watcher survives `hc server stop` (no `HC_SCOPE` sweep on a normal stop).
9. The `/usr/bin/python3` shim injects `SDKROOT`, `CPATH`, `MANPATH` into children (prototype artifact only).

## Changes this forces on DESIGN.md

1. **Rule 3:** run the agent in its own process group as the tty's foreground job (`setpgid`, `tcsetpgrp`, SIGTTOU ignored), take the tty back, restore every signal disposition, then exec the real shell.
2. **Rule 1:** the 2 s budget is one total deadline across connect, write and read. A successful connect proves nothing.
3. **Forget flow:** "shell outlived the agent" is unsafe at shutdown. Primary quit signal: the agent's own end event or the wrapper's exit status. The watcher also checks for a shutdown in progress (loginwindow `shutdownInitiated` / `restartinitiated` notifications on macOS, logind `PrepareForShutdown` on Linux), must die on SIGTERM, and goes on the reboot matrix.
4. **`record`:** every writer, the wrapper included, does compare-and-set on `agent_pid` and `seq`.
5. **`gate`:** key the gate file by boot id plus core pid so a previous boot's file is ignored. Confirmation = the resumed agent's hook re-stamps the record with the child's pid. Keep a confirm timeout.
6. **Wake flow:** document that every kept terminal becomes a wrapper terminal after its first recovery, including ones created with an explicit argv. A record from an agent that *was* the terminal's program (`hc run claude`) has the holder as `shell_pid`; handle that.
7. **`report` / `doctor`:** a broken `shell` is invisible in the API. Detect `keep && live && pid == null` for more than a few seconds and read `holder.log`. A repaired config can take up to 60 s to apply; say so in uninstall and repair output. (Untested: whether `server.restart` forces an immediate pass.)
8. **Boot environment:** `boot.env` also carries `SHELL`, `USER`, `LOGNAME`, `TMPDIR`.
9. **Testing:** the throwaway-home harness sets `socket =` in its config and `SSH_CONNECTION`, or it touches the developer's launchd domain.
10. Rule 4 holds: no `-l` in the argv.
