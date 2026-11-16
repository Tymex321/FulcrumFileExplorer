#!/usr/bin/env bash
# The full pre-commit gate: formatting, lints, tests, the Windows typecheck and the UI.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
step() {
  echo ""
  echo "── $1 ──"
  shift
  if "$@"; then
    echo "   ok"
  else
    echo "   FAILED"
    fail=1
  fi
}

step "rustfmt"          cargo fmt --all --check
step "clippy"           cargo clippy --workspace --all-targets -- -D warnings
step "rust tests"       cargo test --workspace --quiet
# Typechecks every #[cfg(windows)] path without a Windows machine. The fast half of the
# Windows feedback loop.
step "windows typecheck" cargo win

if [ -d ui/node_modules ]; then
  step "ui build" bash -c "cd ui && npm run build"
  step "ui tests" bash -c "cd ui && npx vitest run"
else
  echo ""
  echo "── ui ── skipped (run 'npm --prefix ui install' first)"
fi

echo ""
if [ "$fail" -eq 0 ]; then
  echo "all checks passed"
else
  echo "one or more checks FAILED"
  exit 1
fi
