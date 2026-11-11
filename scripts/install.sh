#!/bin/sh
# Installs herdr-wake, or updates it in place. With REPO as set in main():
#
#   curl -fsSL https://github.com/$REPO/releases/latest/download/install.sh | sh
#
# Arguments go to `herdr-wake install` (use `sh -s --` when piping):
#
#   curl -fsSL .../install.sh | sh -s -- --before-login
#   curl -fsSL .../install.sh | sh -s -- --dry-run
#
# The second one still installs the binary; only the setup step is a dry run.
# To change nothing at all, use HERDR_WAKE_DRY_RUN=1.
#
# Environment:
#   HERDR_WAKE_VERSION=0.1.0  install that release (default: the latest)
#   HERDR_WAKE_BIN_DIR=/path  install there (default: $HOME/.local/bin)
#   HERDR_WAKE_NO_SETUP=1     install the binary only, skip `herdr-wake install`
#   HERDR_WAKE_DRY_RUN=1      print the target, URLs and paths, then stop
#                             (no network, nothing written)
#   HERDR_WAKE_REPO=owner/repo  download from another GitHub repository
#
# Never uses sudo. Every step lives in main(), called on the last line, so a
# download cut short runs nothing.

say() { printf '%s\n' "$*"; }
warn() { printf 'herdr-wake: warning: %s\n' "$*" >&2; }
fail() {
  printf 'herdr-wake install: %s\n' "$*" >&2
  exit 1
}
have() { command -v "$1" >/dev/null 2>&1; }

# Sets os_name, cpu and target, or refuses with a clear message.
detect_target() {
  os_name=$(uname -s)
  machine=$(uname -m)
  case "$os_name" in
    Darwin) os_part=apple-darwin ;;
    Linux) os_part=unknown-linux-musl ;;
    MINGW* | MSYS* | CYGWIN* | Windows_NT)
      fail "Windows support is coming soon. herdr-wake runs on macOS and Linux for now." ;;
    *) fail "there is no herdr-wake build for $os_name. Supported: macOS and Linux (Windows coming soon)." ;;
  esac
  case "$machine" in
    arm64 | aarch64) cpu=aarch64 ;;
    x86_64 | amd64) cpu=x86_64 ;;
    *) fail "there is no herdr-wake build for the $machine CPU. Supported: arm64 (aarch64) and x86_64." ;;
  esac
  # A shell running under Rosetta reports x86_64 on Apple silicon: use the
  # native build.
  if [ "$os_name" = Darwin ] && [ "$cpu" = x86_64 ] &&
    [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
    cpu=aarch64
  fi
  target="$cpu-$os_part"
}

check_version() {
  case "$1" in
    "" | *[!0-9A-Za-z.-]*) fail "not a usable version: '$1' (expected something like 0.1.0)" ;;
  esac
}

# Follows GitHub's /releases/latest redirect to the newest stable tag.
resolve_latest() {
  latest_url="https://github.com/$REPO/releases/latest"
  final=$(curl -fsSLI --proto '=https' -o /dev/null -w '%{url_effective}' "$latest_url") ||
    fail "cannot reach $latest_url (is the repository public and does it have a release?)"
  tag=${final##*/}
  case "$tag" in
    v[0-9]*) version=${tag#v} ;;
    *) fail "no release found at https://github.com/$REPO/releases (latest pointed to $final)" ;;
  esac
  check_version "$version"
}

sha256_of() {
  if have sha256sum; then
    sha256sum "$1"
  else
    shasum -a 256 "$1"
  fi | awk '{ print $1; exit }'
}

# Runs a command with the user's terminal as stdin. When this script is piped
# into sh, stdin is the script itself; prompts must read the terminal instead.
with_tty() {
  if [ -t 0 ]; then
    "$@"
  elif (exec </dev/tty) 2>/dev/null; then
    "$@" </dev/tty
  else
    "$@" </dev/null
  fi
}

main() {
  set -eu
  REPO="${HERDR_WAKE_REPO:-Heretikio/herdr-wake}"
  tmp=""
  staged=""

  if [ "$(id -u)" = 0 ] && [ -n "${SUDO_USER:-}" ]; then
    fail "do not run this with sudo. herdr-wake installs for your own user; run it again without sudo."
  fi

  detect_target

  [ -n "${HERDR_WAKE_BIN_DIR:-}" ] || [ -n "${HOME:-}" ] ||
    fail "HOME is not set; set HERDR_WAKE_BIN_DIR to the folder to install into"
  bin_dir=${HERDR_WAKE_BIN_DIR:-$HOME/.local/bin}
  bin_dir=${bin_dir%/}
  case "$bin_dir" in
    /*) ;;
    *) fail "HERDR_WAKE_BIN_DIR must be an absolute path (got '$bin_dir')" ;;
  esac
  dest="$bin_dir/herdr-wake"

  if have sha256sum; then
    sum_tool=sha256sum
  elif have shasum; then
    sum_tool="shasum -a 256"
  else
    sum_tool=""
  fi

  on_path=no
  case ":${PATH:-}:" in *":$bin_dir:"*) on_path=yes ;; esac

  pinned=${HERDR_WAKE_VERSION:-}
  pinned=${pinned#v}
  [ -z "$pinned" ] || check_version "$pinned"

  if [ "${HERDR_WAKE_DRY_RUN:-0}" = 1 ]; then
    if [ -n "$pinned" ]; then
      shown_version="$pinned (pinned by HERDR_WAKE_VERSION)"
      v=$pinned
    else
      shown_version="latest (resolved at install time from https://github.com/$REPO/releases/latest)"
      v="<latest>"
    fi
    base="https://github.com/$REPO/releases/download/v$v"
    asset="herdr-wake-$v-$target.tar.gz"
    if [ "${HERDR_WAKE_NO_SETUP:-0}" = 1 ]; then
      setup="skipped (HERDR_WAKE_NO_SETUP=1)"
    else
      setup="$dest install${*:+ $*}"
    fi
    say "herdr-wake install: dry run (nothing downloaded or written)"
    say "  system:     $os_name $machine"
    say "  target:     $target"
    say "  repository: $REPO"
    say "  version:    $shown_version"
    say "  archive:    $base/$asset"
    say "  checksum:   $base/$asset.sha256"
    say "  verify:     ${sum_tool:-MISSING (install would refuse: needs sha256sum or shasum)}"
    say "  install to: $dest"
    say "  temp file:  $bin_dir/.herdr-wake.new.<pid> (renamed into place)"
    say "  on PATH:    $on_path"
    say "  then run:   $setup"
    return 0
  fi

  have curl || fail "needs curl"
  have tar || fail "needs tar"
  [ -n "$sum_tool" ] ||
    fail "needs sha256sum or shasum to verify the download; refusing to install an unverified binary"

  if [ -n "$pinned" ]; then
    version=$pinned
  else
    resolve_latest
  fi

  base="https://github.com/$REPO/releases/download/v$version"
  asset="herdr-wake-$version-$target.tar.gz"

  # Removes the download folder and any half-made temp file on every exit.
  trap '[ -z "$tmp" ] || rm -rf "$tmp"; [ -z "$staged" ] || rm -f "$staged"' EXIT
  trap 'exit 130' INT TERM
  tmp=$(mktemp -d 2>/dev/null || mktemp -d -t herdr-wake)

  say "Downloading herdr-wake $version for $target"
  curl -fsSL --proto '=https' -o "$tmp/$asset" "$base/$asset" ||
    fail "download failed: $base/$asset"
  curl -fsSL --proto '=https' -o "$tmp/$asset.sha256" "$base/$asset.sha256" ||
    fail "checksum download failed: $base/$asset.sha256 (refusing to install without it)"

  # What this checksum is for: it guards against corruption (a download
  # that was cut short or damaged on the way),
  # not against a compromised release.
  # The .sha256 file comes from the same release as the archive,
  # so whoever could replace one could replace both. To check where a build
  # came from, use the build provenance attestation:
  #   gh attestation verify <archive> --repo $REPO
  expected=$(awk '{ print tolower($1); exit }' "$tmp/$asset.sha256")
  case "$expected" in
    *[!0-9a-f]* | "") fail "the checksum file is malformed: $base/$asset.sha256" ;;
  esac
  [ "${#expected}" -eq 64 ] || fail "the checksum file is malformed: $base/$asset.sha256"
  actual=$(sha256_of "$tmp/$asset")
  [ "$actual" = "$expected" ] ||
    fail "checksum mismatch for $asset (expected $expected, got $actual). Nothing was installed; try again."
  say "Checksum verified ($sum_tool)"

  tar -xzf "$tmp/$asset" -C "$tmp" herdr-wake ||
    fail "the archive does not contain herdr-wake: $base/$asset"
  [ -f "$tmp/herdr-wake" ] || fail "the archive does not contain herdr-wake: $base/$asset"
  "$tmp/herdr-wake" --help >/dev/null 2>&1 ||
    fail "the downloaded herdr-wake does not run on this machine ($target)"

  mkdir -p "$bin_dir"
  action=Installed
  [ ! -e "$dest" ] || action=Updated
  # Temp file in the same folder, then one rename: a running copy (core's
  # configured shell) is never left half-written.
  # mktemp picks a name nobody could have planted (a file or a symlink
  # waiting at a predictable name would otherwise be written through).
  staged=$(mktemp "$bin_dir/.herdr-wake.new.XXXXXX") ||
    fail "could not create a temporary file in $bin_dir"
  cp "$tmp/herdr-wake" "$staged"
  chmod 755 "$staged"
  if [ "$os_name" = Darwin ] && have xattr &&
    xattr -p com.apple.quarantine "$staged" >/dev/null 2>&1; then
    xattr -d com.apple.quarantine "$staged"
    say "Cleared the macOS quarantine flag"
  fi
  mv -f "$staged" "$dest"
  staged=""
  say "$action herdr-wake $version at $dest"

  if [ "$on_path" = no ]; then
    warn "$bin_dir is not on your PATH. Terminals still work (core is given the full path); to run herdr-wake by name, add this to your shell profile: export PATH=\"$bin_dir:\$PATH\""
  else
    first=$(command -v herdr-wake 2>/dev/null || true)
    if [ -n "$first" ] && [ "$first" != "$dest" ]; then
      warn "another herdr-wake comes first on your PATH: $first"
    fi
  fi

  if [ "${HERDR_WAKE_NO_SETUP:-0}" = 1 ]; then
    say "Skipped setup (HERDR_WAKE_NO_SETUP=1). To finish: $dest install"
    return 0
  fi
  say "Running: $dest install${*:+ $*}"
  with_tty "$dest" install "$@" ||
    fail "'herdr-wake install' failed (see above). The binary is installed at $dest; fix the problem and run '$dest install' again, or '$dest doctor'."
}

main "$@"
