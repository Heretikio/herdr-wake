#!/bin/sh
# Removes herdr-wake. With REPO as set in main():
#
#   curl -fsSL https://github.com/$REPO/releases/latest/download/uninstall.sh | sh
#
# Runs `herdr-wake uninstall` first (it restores herdr-core's own shell
# setting, the boot service and the agent hooks), and removes the binary only
# when that succeeded. Arguments go to `herdr-wake uninstall`; with --dry-run
# the binary is kept too.
#
# Environment:
#   HERDR_WAKE_BIN=/path/herdr-wake  the binary to run and remove (default: the one in HERDR_WAKE_BIN_DIR)
#   HERDR_WAKE_BIN_DIR=/path  where install.sh put the binary (default: $HOME/.local/bin)
#   HERDR_WAKE_REPO=owner/repo  only used in messages
#
# Never uses sudo. Every step lives in main(), called on the last line, so a
# download cut short runs nothing.

say() { printf '%s\n' "$*"; }
fail() {
  printf 'herdr-wake uninstall: %s\n' "$*" >&2
  exit 1
}

# Runs a command with the user's terminal as stdin (see install.sh).
with_tty() {
  if [ -t 0 ]; then
    "$@"
  elif (exec </dev/tty) 2>/dev/null; then
    "$@" </dev/tty
  else
    "$@" </dev/null
  fi
}

# Prints the first executable herdr-wake in the absolute folders of PATH.
# Relative and empty entries (the current folder) are skipped: a file that
# happens to sit where this script is run must never be executed.
find_on_path() {
  old_ifs=$IFS
  IFS=:
  for dir in ${PATH:-}; do
    case "$dir" in
      /*)
        if [ -f "${dir%/}/herdr-wake" ] && [ -x "${dir%/}/herdr-wake" ]; then
          IFS=$old_ifs
          printf '%s\n' "${dir%/}/herdr-wake"
          return 0
        fi
        ;;
    esac
  done
  IFS=$old_ifs
  return 0
}

main() {
  set -eu
  REPO="${HERDR_WAKE_REPO:-Heretikio/herdr-wake}"

  if [ "$(id -u)" = 0 ] && [ -n "${SUDO_USER:-}" ]; then
    fail "do not run this with sudo. herdr-wake is installed per user; run it again without sudo."
  fi

  [ -n "${HERDR_WAKE_BIN_DIR:-}" ] || [ -n "${HOME:-}" ] ||
    fail "HOME is not set; set HERDR_WAKE_BIN_DIR to the folder herdr-wake is in"
  bin_dir=${HERDR_WAKE_BIN_DIR:-$HOME/.local/bin}
  # Which herdr-wake to run and then remove, in this order: the one named
  # by HERDR_WAKE_BIN, the one in the install folder, and only then the one
  # on PATH. A PATH match must be an absolute path (a relative PATH entry
  # could name a file in the current folder) and is shown before it runs.
  if [ -n "${HERDR_WAKE_BIN:-}" ]; then
    bin=$HERDR_WAKE_BIN
    case "$bin" in
      /*) ;;
      *) fail "HERDR_WAKE_BIN must be an absolute path: $bin" ;;
    esac
    [ -x "$bin" ] || fail "HERDR_WAKE_BIN names $bin, which is not an executable file"
  else
    bin="${bin_dir%/}/herdr-wake"
    if [ ! -x "$bin" ]; then
      bin=$(find_on_path)
      [ -n "$bin" ] || fail "herdr-wake is not in $bin_dir or in an absolute folder of your PATH; nothing to uninstall. Set HERDR_WAKE_BIN to its full path if it is somewhere else."
      say "herdr-wake is not in $bin_dir; found it on your PATH at $bin"
    fi
  fi

  dry=0
  for arg in "$@"; do
    case "$arg" in --dry-run) dry=1 ;; esac
  done

  say "Running: $bin uninstall${*:+ $*}"
  status=0
  with_tty "$bin" uninstall "$@" || status=$?
  if [ "$status" -ne 0 ]; then
    fail "'herdr-wake uninstall' failed (exit $status), so the binary was NOT removed: $bin. It may still be herdr-core's configured shell, and removing it now would leave every herdr terminal without a working shell. Fix the problem above and run this again, or report it at https://github.com/$REPO/issues"
  fi

  if [ "$dry" = 1 ]; then
    say "Dry run: would remove $bin"
    return 0
  fi

  case "$bin" in
    */Cellar/* | /opt/homebrew/* | /home/linuxbrew/*)
      say "Kept $bin: a package manager installed it. Remove it with that (for example: brew uninstall herdr-wake)."
      return 0
      ;;
  esac

  if [ "$(uname -s)" = Darwin ]; then
    trash="$HOME/.Trash"
    mkdir -p "$trash"
    name=herdr-wake
    [ ! -e "$trash/$name" ] || name="herdr-wake $(date '+%Y-%m-%d %H.%M.%S')"
    mv -f "$bin" "$trash/$name" || fail "could not move $bin to the Trash; remove it yourself"
    say "Moved $bin to the Trash ($trash/$name)"
  else
    rm -f "$bin" || fail "could not remove $bin; remove it yourself"
    say "Removed $bin"
  fi
  say "herdr-wake is uninstalled."
}

main "$@"
