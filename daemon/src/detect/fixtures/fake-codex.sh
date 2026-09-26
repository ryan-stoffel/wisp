#!/bin/sh
# A synthetic stand-in for the codex CLI in detection tests. Its output is invented.
#
# `codex app-server` reads one NDJSON request from stdin, replies with $FAKE_CLI_APP_SERVER_RESPONSE
# (if set), then hangs like a real long-lived server until its group is killed. Anything else
# behaves like `codex login status`: stdout and exit code from the FAKE_CLI_* variables.
if [ "$1" = "app-server" ]; then
  IFS= read -r _request
  if [ -n "${FAKE_CLI_APP_SERVER_RESPONSE:-}" ]; then printf '%s\n' "$FAKE_CLI_APP_SERVER_RESPONSE"; fi
  while :; do sleep 60 & wait $!; done
fi
printf '%s' "${FAKE_CLI_STDOUT:-}"
exit "${FAKE_CLI_EXIT:-0}"
