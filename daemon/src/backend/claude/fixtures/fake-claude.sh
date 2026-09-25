#!/bin/sh
# A stand-in for the claude CLI in tests. It records its arguments, its environment (whose PWD
# is its working directory), and stdin under $FAKE_CLAUDE_DIR, then replays $FAKE_CLAUDE_FIXTURE
# line by line.
#
# Fixture lines starting with # are comments and blank lines are skipped. These directives act:
#   @read          read one line of stdin, or exit 0 if stdin has ended
#   @eof           read stdin until it ends
#   @sleep <s>     sleep
#   @stderr <text> write a line to stderr
#   @exit <code>   exit
#   @trap-int      on SIGINT, record it and exit 130
#   @ignore-int    ignore SIGINT
#   @hang          wait forever
# Every other line goes to stdout as it is.

dir=$FAKE_CLAUDE_DIR
printf '%s\n' "$@" > "$dir/argv"
env > "$dir/env"
: > "$dir/stdin"
exec 3< "$FAKE_CLAUDE_FIXTURE"
while IFS= read -r line <&3; do
  case $line in
    '#'* | '') ;;
    '@read') IFS= read -r input || exit 0; printf '%s\n' "$input" >> "$dir/stdin" ;;
    '@eof') while IFS= read -r input; do printf '%s\n' "$input" >> "$dir/stdin"; done ;;
    '@sleep '*) sleep "${line#@sleep }" ;;
    '@stderr '*) printf '%s\n' "${line#@stderr }" >&2 ;;
    '@exit '*) exit "${line#@exit }" ;;
    '@trap-int') trap 'echo SIGINT >> "$dir/signals"; exit 130' INT ;;
    '@ignore-int') trap '' INT ;;
    '@hang') while :; do sleep 60 & wait $!; done ;;
    *) printf '%s\n' "$line" ;;
  esac
done
exit 0
