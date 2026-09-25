#!/bin/sh
# A synthetic stand-in for the claude CLI's `auth status` in detection tests (#114). Its output is
# invented, not captured from the real CLI; see #124 for recorded transcripts.
if [ -n "${FAKE_CLI_SLEEP:-}" ]; then sleep "$FAKE_CLI_SLEEP"; fi
if [ -n "${FAKE_CLI_STDERR:-}" ]; then printf '%s' "$FAKE_CLI_STDERR" >&2; fi
printf '%s' "${FAKE_CLI_STDOUT:-}"
exit "${FAKE_CLI_EXIT:-0}"
