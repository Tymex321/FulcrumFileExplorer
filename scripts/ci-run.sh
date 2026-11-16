#!/usr/bin/env bash
# Runs one CI step and, on failure, puts the first errors in the job summary.
#
#   scripts/ci-run.sh "<title>" <command...>
#
# A dense rustc error dump inside a collapsed matrix job is close to useless, and on the
# Windows leg it is the whole point of CI. So the full log still streams to the job log,
# but the first few error blocks are also written to $GITHUB_STEP_SUMMARY and emitted as
# annotations, which show on the run's front page and on the PR diff.
set -uo pipefail

title="$1"
shift
log="$(mktemp)"
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"

"$@" 2>&1 | tee "$log"
status="${PIPESTATUS[0]}"
[ "$status" -eq 0 ] && { rm -f "$log"; exit 0; }

# rustc/clippy start a diagnostic with "error" or "error[E....]" at column 0 and end it at
# the next blank line. Keep the first 5 blocks (plus the "-->" location line they carry);
# if nothing matches (npm, vitest, rustfmt) fall back to the log tail.
errors="$(awk '
  /^error/ && !/^error: aborting/ && !/^error: could not compile/ { if (n < 5) { n++; show = 1 } }
  show { print }
  /^$/ { show = 0 }
' "$log")"
# A failing `cargo test` prints one "---- <name> stdout ----" block per test, with blank
# lines inside, ending at the "failures:" list. Keep the first 5 of those too.
tests="$(awk '
  /^---- .* stdout ----$/ { n++; on = (n <= 5) }
  /^failures:$/ { on = 0 }
  on { print }
' "$log")"
[ -n "$tests" ] && errors="${tests}"$'\n'"${errors}"
[ -z "$errors" ] && errors="$(tail -n 40 "$log")"

{
  echo "### ❌ ${title} failed (exit ${status}) on ${RUNNER_OS:-local}"
  echo
  echo '```'
  echo "$errors" | head -n 120
  echo '```'
  echo
} >> "$summary"

# One annotation per located error so they land on the files in the PR.
echo "$errors" | grep -E '^\s+--> ' | head -n 10 | while read -r _ loc; do
  file="${loc%%:*}"
  rest="${loc#*:}"
  echo "::error file=${file},line=${rest%%:*},title=${title}::see job summary"
done

# One annotation per failed test, carrying its output, so the failure is readable from the
# run's front page without opening the log. Each test block is joined onto one line with
# \x1f, which becomes the %0A newline escape that workflow commands require.
if [ -n "$tests" ]; then
  printf '%s\n' "$tests" |
    awk '/^---- .* stdout ----$/ { if (b) print b; b = $0; next } { b = b "\037" $0 } END { if (b) print b }' |
    while IFS= read -r block; do
      msg="${block:0:3000}"
      msg="${msg//'%'/%25}"
      msg="${msg//$'\r'/%0D}"
      msg="${msg//$'\037'/%0A}"
      echo "::error title=${title}::${msg}"
    done
fi

rm -f "$log"
exit "$status"
