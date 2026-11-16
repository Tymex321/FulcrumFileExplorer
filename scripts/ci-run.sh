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

rm -f "$log"
exit "$status"
