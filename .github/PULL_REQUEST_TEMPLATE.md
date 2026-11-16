## What and why

<!-- What changed, and the reason. The why matters more than the what. -->

## Checks

- [ ] `./scripts/check-all.sh` passes locally (fmt, clippy, tests, `cargo win`, UI)
- [ ] CI is green on **both** `macos-latest` and `windows-latest`
- [ ] No `unwrap()` in production code
- [ ] Dependency versions live in the root `[workspace.dependencies]`, not in a crate manifest
- [ ] If a type in `crates/fx-core` changed, `ui/src/bindings` was regenerated (`scripts/gen-bindings.sh`)

## Windows acceptance pass

Only CI's `windows-latest` runner compiles the Windows half, and nothing in CI can exercise
the raw volume or UAC. Tick what applies to this change, on a real Windows machine:

- [ ] App launches **without** a UAC prompt (the main app is `asInvoker`; only the helper elevates)
- [ ] Without the helper: search still works through the fallback and the UI offers "Enable instant search"
- [ ] Enabling instant search shows exactly one UAC prompt, then the index builds
- [ ] Whole-volume search returns results as you type (note the latency you saw: ____)
- [ ] Creating, renaming and deleting a file shows up in search without a rescan (USN journal)
- [ ] Listing a very large directory paints its first rows immediately
- [ ] Copy/move shows real progress and a believable ETA; conflicts prompt correctly
- [ ] The NSIS installer installs, launches, upgrades and uninstalls cleanly
- [ ] Not applicable (docs, CI or UI-only change)

## Benchmarks

Link the `⚡ Query benchmarks` section of the CI job summary if this touches `fx-index`.
Regressions never fail the build, so someone has to look.
