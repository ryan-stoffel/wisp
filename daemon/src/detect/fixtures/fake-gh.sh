#!/bin/sh
# A synthetic stand-in for the GitHub CLI in detection and setup tests (PLX-336, PLX-423). Its
# output is invented, except `auth login`'s two code lines, which are copied from gh 2.102.0's
# non-interactive `--web` output.
#
# `gh --version` prints a version banner. `gh auth login` prints a one-time code to stderr, writes
# its pid to FAKE_GH_PIDFILE if set, then waits: until the file FAKE_GH_APPROVE exists (exit 0),
# or with FAKE_GH_LOGIN=fail, exits 1 at once. `gh auth setup-git` creates the file
# FAKE_GH_SETUP_GIT_MARK if set, and exits FAKE_GH_SETUP_GIT_EXIT (default 0). Anything else
# behaves like `gh auth status`: stdout and exit from the FAKE_CLI_* variables.
# FAKE_GH_VERSION sets the version. At 2.45.0 it refuses `--active`, as gh before 2.57.0 does.
version="${FAKE_GH_VERSION:-2.100.0}"
if [ "$1" = "--version" ]; then
  printf 'gh version %s (2026-09-01)\nhttps://github.com/cli/cli/releases/tag/v%s\n' "$version" "$version"
  exit 0
fi
if [ "$1 $2" = "auth login" ]; then
  [ -n "$FAKE_GH_PIDFILE" ] && echo $$ > "$FAKE_GH_PIDFILE"
  printf '\n! One-time code (AB12-CD34) copied to clipboard\n' >&2
  printf 'Open this URL to continue in your web browser: https://github.com/login/device\n' >&2
  if [ "$FAKE_GH_LOGIN" = "fail" ]; then
    echo 'error: the device code expired' >&2
    exit 1
  fi
  while [ ! -e "${FAKE_GH_APPROVE:-/nonexistent}" ]; do sleep 0.1; done
  echo '✓ Authentication complete.' >&2
  exit 0
fi
if [ "$1 $2" = "auth setup-git" ]; then
  [ -n "$FAKE_GH_SETUP_GIT_MARK" ] && : > "$FAKE_GH_SETUP_GIT_MARK"
  if [ "${FAKE_GH_SETUP_GIT_EXIT:-0}" != 0 ]; then
    echo 'failed to set up git credential helper: read-only file system' >&2
  fi
  exit "${FAKE_GH_SETUP_GIT_EXIT:-0}"
fi
for arg in "$@"; do
  if [ "$arg" = "--active" ] && [ "$version" = "2.45.0" ]; then
    echo 'unknown flag: --active' >&2
    exit 1
  fi
done
printf '%s' "${FAKE_CLI_STDOUT:-}"
exit "${FAKE_CLI_EXIT:-0}"
