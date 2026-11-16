#!/usr/bin/env bash
# Regenerates ui/src/bindings/ from the #[derive(TS)] types in fx-core.
#
# ts-rs emits the TypeScript as a side effect of running the tests, with the output
# directory set by TS_RS_EXPORT_DIR in .cargo/config.toml. Run this after any change to a
# type that crosses the IPC boundary — the generated types are what turn a Rust/TS
# mismatch into a compile error instead of a runtime surprise.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo test -p fx-core --quiet
count=$(find ui/src/bindings -name '*.ts' 2>/dev/null | wc -l | tr -d ' ')
echo "regenerated ${count} TypeScript types in ui/src/bindings/"
