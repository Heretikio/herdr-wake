#!/bin/bash
# Stub coding agent for the integration suite. One script stands in for
# claude, codex, pi and opencode: the harness installs a launcher per agent
# that starts this file with argv[0] set to the agent's name and STUB_KIND
# set, so the process looks like the agent to herdr-wake.
#
# It does what the real agent's integration does and nothing else:
#   1. works out its session id (a new one, or the one it is asked to resume),
#   2. creates the session file the adapter checks for (new sessions only),
#   3. runs `herdr-wake hook <agent> start` with a realistic payload,
#   4. appends one line to $HOME/stub/starts.log (the test's marker),
#   5. waits until $HOME/stub/quit-<terminal> appears, then runs the end hook
#      where the real agent has one and exits 0. A signal just kills it.
#
# Controls, all files under $HOME/stub:
#   herdr-wake          path of the herdr-wake binary under test (required)
#   fail-<terminal>     a resumed stub in that terminal exits 1 at once
#   quit-<terminal>     the stub in that terminal quits like a person would
#   nohook              stubs call no start hook (an agent from before install)
# Environment: STUB_KIND (required), STUB_ID (session id for a new session).

set -u
kind=${STUB_KIND:?}
# `<agent> --version`, as herdr-wake's install and doctor ask it.
if [ "${1:-}" = --version ]; then
  echo "${STUB_VERSION:-1.2.3}"
  exit 0
fi
stub=$HOME/stub
term=${HC_TERMINAL_ID:-none}
hw=$(cat "$stub/herdr-wake")
cwd=$(pwd -P)

now_ms() { perl -MTime::HiRes=time -e 'printf "%d\n", time * 1000'; }

# --- session id and launch flags -------------------------------------------
resumed=0
id=
file=
flags=()
args=("$@")
n=${#args[@]}
i=0
while [ "$i" -lt "$n" ]; do
  a=${args[$i]}
  case "$kind:$a" in
    claude:--resume)
      resumed=1; i=$((i + 1)); id=${args[$i]} ;;
    codex:resume)
      # codex resume <flags> <id>: the id is the last word
      resumed=1; id=${args[$((n - 1))]}; n=$((n - 1)) ;;
    pi:--session)
      resumed=1; i=$((i + 1)); file=${args[$i]}
      id=$(basename "$file" .jsonl) ;;
    opencode:--session)
      resumed=1; i=$((i + 1)); id=${args[$i]} ;;
    *)
      flags+=("$a") ;;
  esac
  i=$((i + 1))
done
if [ -z "$id" ]; then
  id=${STUB_ID:-$kind-$term-$$}
fi

log_start() {
  # tab-separated key=value fields; argv words are joined with a space
  printf 'ms=%s\tterminal=%s\tkind=%s\tpid=%s\tresumed=%s\tid=%s\tcwd=%s\tsupervised=%s\tconfig_dir=%s\targv=%s\n' \
    "$(now_ms)" "$term" "$kind" "$$" "$resumed" "$id" "$cwd" \
    "${HERDR_WAKE_SUPERVISED:-}" "${CLAUDE_CONFIG_DIR:-}" "$*" >> "$stub/starts.log"
}

if [ "$resumed" = 1 ] && [ -e "$stub/fail-$term" ]; then
  log_start "$@"
  echo "STUB $kind failing on purpose"
  exit 1
fi

# --- the session file the adapter checks ------------------------------------
case "$kind" in
  claude)
    dir=${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects/$(printf %s "$cwd" | sed 's/[^a-zA-Z0-9]/-/g')
    file=$dir/$id.jsonl ;;
  codex)
    dir=${CODEX_HOME:-$HOME/.codex}/sessions/2026/11/10
    file=$dir/rollout-2026-11-10T09-15-02-$id.jsonl ;;
  pi)
    dir=$HOME/.pi/agent/sessions/stub
    [ -n "$file" ] || file=$dir/$id.jsonl ;;
  opencode)
    dir=; file= ;;
esac
if [ "$resumed" = 0 ] && [ -n "$file" ]; then
  mkdir -p "$dir"
  if [ "$kind" = codex ]; then
    printf '{"type":"session_meta","payload":{"id":"%s","source":"cli","cwd":"%s"}}\n' "$id" "$cwd" > "$file"
  else
    printf '{"type":"stub","id":"%s"}\n' "$id" > "$file"
  fi
fi

# --- the start hook ---------------------------------------------------------
json_argv() {
  local out="\"$kind\"" w
  for w in "$@"; do out="$out,\"$w\""; done
  printf '[%s]' "$out"
}
source=startup
[ "$resumed" = 1 ] && source=resume
case "$kind" in
  claude)
    payload=$(printf '{"session_id":"%s","transcript_path":"%s","cwd":"%s","hook_event_name":"SessionStart","source":"%s"}' "$id" "$file" "$cwd" "$source") ;;
  codex)
    payload=$(printf '{"session_id":"%s","cwd":"%s","transcript_path":"%s","hook_event_name":"SessionStart","source":"%s"}' "$id" "$cwd" "$file" "$source") ;;
  pi)
    payload=$(printf '{"reason":"%s","id":"%s","file":"%s","cwd":"%s","argv":%s,"pid":%s}' "$source" "$id" "$file" "$cwd" "$(json_argv ${flags[@]+"${flags[@]}"})" "$$") ;;
  opencode)
    payload=$(printf '{"id":"%s","cwd":"%s","argv":%s,"pid":%s,"major":1}' "$id" "$cwd" "$(json_argv ${flags[@]+"${flags[@]}"})" "$$") ;;
esac
if [ "$kind" = claude ]; then
  # Claude's own per-process session file, which `herdr-wake adopt` reads.
  sessions=${CLAUDE_CONFIG_DIR:-$HOME/.claude}/sessions
  mkdir -p "$sessions"
  printf '{"pid":%s,"sessionId":"%s","cwd":"%s","kind":"interactive","entrypoint":"cli"}\n' "$$" "$id" "$cwd" > "$sessions/$$.json"
fi
if [ ! -e "$stub/nohook" ]; then
  printf %s "$payload" | "$hw" hook "$kind" start
fi

log_start "$@"
if [ "$resumed" = 1 ]; then
  echo "STUB $kind resumed session=$id"
else
  echo "STUB $kind started session=$id"
fi

# --- wait for a quit --------------------------------------------------------
while [ ! -e "$stub/quit-$term" ]; do
  sleep 0.1
done
case "$kind" in
  claude)
    printf '{"session_id":"%s","hook_event_name":"SessionEnd","reason":"prompt_input_exit"}' "$id" | "$hw" hook claude end ;;
  pi | opencode)
    printf '{"id":"%s","signal":false}' "$id" | "$hw" hook "$kind" end ;;
  codex) ;; # Codex has no end hook: the watcher or the wrapper decides
esac
echo "STUB $kind bye"
exit 0
