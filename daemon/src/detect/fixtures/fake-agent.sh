#!/bin/sh
# A synthetic stand-in for Cursor's `agent` CLI in detection tests (#114). Its output is invented,
# not captured from the real CLI; see #124 for recorded transcripts.
#
# `agent about` replies from the FAKE_CLI_ABOUT_* variables; anything else (`agent status
# --format json`) replies from the FAKE_CLI_* variables.
if [ "$1" = "about" ]; then
  if [ -n "${FAKE_CLI_ABOUT_SLEEP:-}" ]; then sleep "$FAKE_CLI_ABOUT_SLEEP"; fi
  printf '%s' "${FAKE_CLI_ABOUT_STDOUT:-}"
  exit "${FAKE_CLI_ABOUT_EXIT:-0}"
fi
if [ -n "${FAKE_CLI_SLEEP:-}" ]; then sleep "$FAKE_CLI_SLEEP"; fi
if [ -n "${FAKE_CLI_STDERR:-}" ]; then printf '%s' "$FAKE_CLI_STDERR" >&2; fi
printf '%s' "${FAKE_CLI_STDOUT:-}"
exit "${FAKE_CLI_EXIT:-0}"
