#!/bin/sh
# A synthetic stand-in for the claude CLI's `auth status` in detection tests. Its output is
# invented.
if [ -n "${FAKE_CLI_SLEEP:-}" ]; then sleep "$FAKE_CLI_SLEEP"; fi
printf '%s' "${FAKE_CLI_STDOUT:-}"
exit "${FAKE_CLI_EXIT:-0}"
