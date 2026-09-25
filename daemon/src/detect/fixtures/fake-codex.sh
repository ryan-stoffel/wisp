#!/bin/sh
# A synthetic stand-in for the codex CLI in detection tests (#114). Its output is invented, not
# captured from the real CLI; see #124 for recorded transcripts.
#
# `codex app-server` reads one NDJSON request from stdin, replies with $FAKE_CLI_APP_SERVER_RESPONSE
# (if set), then hangs like a real long-lived server until its group is killed. Anything else
# behaves like `codex login status`: sleep, then stderr/stdout/exit from the FAKE_CLI_* variables.
if [ "$1" = "app-server" ]; then
  IFS= read -r _request
  if [ -n "${FAKE_CLI_APP_SERVER_SLEEP:-}" ]; then sleep "$FAKE_CLI_APP_SERVER_SLEEP"; fi
  if [ -n "${FAKE_CLI_APP_SERVER_RESPONSE:-}" ]; then printf '%s\n' "$FAKE_CLI_APP_SERVER_RESPONSE"; fi
  while :; do sleep 60 & wait $!; done
fi
if [ -n "${FAKE_CLI_SLEEP:-}" ]; then sleep "$FAKE_CLI_SLEEP"; fi
if [ -n "${FAKE_CLI_STDERR:-}" ]; then printf '%s' "$FAKE_CLI_STDERR" >&2; fi
printf '%s' "${FAKE_CLI_STDOUT:-}"
exit "${FAKE_CLI_EXIT:-0}"
