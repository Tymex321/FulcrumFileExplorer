#!/usr/bin/env bash
# Renders criterion's results as a markdown table for $GITHUB_STEP_SUMMARY.
#
#   CRITERION_HOME=<dir> scripts/ci-bench-summary.sh [target-ms]
#
# Reads criterion's JSON rather than scraping its text output, so a format tweak in the
# terminal report cannot silently blank the summary. Never exits non-zero for a *slow*
# number — shared runners are too noisy for that to be a useful gate — but the headline is
# flagged loudly when it is over target, and a vs-baseline column shows regressions.
set -euo pipefail
export LC_ALL=C # awk would otherwise print decimal commas under some locales

root="${CRITERION_HOME:-target/criterion}"
target_ms="${1:-50}"

command -v jq >/dev/null || { echo "jq is required"; exit 1; }

files="$(find "$root" -path '*/new/benchmark.json' 2>/dev/null | sort)"
if [ -z "$files" ]; then
  echo "### ⚠️ Benchmarks produced no results"
  echo
  echo "No criterion output found under \`$root\`."
  exit 0
fi

fmt_ns() {
  awk -v ns="$1" 'BEGIN {
    if (ns >= 1e9) printf "%.2f s", ns / 1e9;
    else if (ns >= 1e6) printf "%.2f ms", ns / 1e6;
    else if (ns >= 1e3) printf "%.2f µs", ns / 1e3;
    else printf "%.0f ns", ns }'
}

rows=""
headline=""
while read -r bench; do
  dir="$(dirname "$bench")"
  id="$(jq -r '.full_id' "$bench")"
  est="$dir/estimates.json"
  [ -f "$est" ] || continue
  # slope is criterion's preferred estimator; flat-sampled benches only have mean.
  point="$(jq -r '(.slope // .mean).point_estimate' "$est")"
  lo="$(jq -r '(.slope // .mean).confidence_interval.lower_bound' "$est")"
  hi="$(jq -r '(.slope // .mean).confidence_interval.upper_bound' "$est")"

  change="n/a"
  chg="$dir/../change/estimates.json"
  if [ -f "$chg" ]; then
    change="$(jq -r '(.mean.point_estimate * 100) as $p | (if $p > 0 then "+" else "" end) + (($p * 100 | round) / 100 | tostring) + "%"' "$chg")"
  fi

  rows+="| \`$id\` | **$(fmt_ns "$point")** | $(fmt_ns "$lo") – $(fmt_ns "$hi") | $change |"$'\n'
  # The first substring bench is the project's headline claim.
  if [ -z "$headline" ] && [[ "$id" == */substring ]]; then
    headline="$id|$point"
  fi
done <<< "$files"

echo "## ⚡ Query benchmarks"
echo
if [ -n "$headline" ]; then
  hid="${headline%%|*}"
  hns="${headline##*|}"
  over="$(awk -v ns="$hns" -v t="$target_ms" 'BEGIN { print (ns / 1e6 > t) ? 1 : 0 }')"
  if [ "$over" = 1 ]; then
    echo "# 🐢 $(fmt_ns "$hns") — over the ${target_ms} ms target"
  else
    echo "# 🚀 $(fmt_ns "$hns") per query (target < ${target_ms} ms)"
  fi
  echo
  echo "\`$hid\`, 1M-entry synthetic index. Informational only: shared-runner variance means this never fails the build."
  echo
fi
echo "| benchmark | time | 95% CI | vs previous run |"
echo "|---|---|---|---|"
printf '%s' "$rows"
