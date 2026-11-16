# Fulcrum

A fast file explorer for Windows, written in Rust.

[![CI](https://github.com/Tymex321/FulcrumFileExplorer/actions/workflows/ci.yml/badge.svg)](https://github.com/Tymex321/FulcrumFileExplorer/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/Tymex321/FulcrumFileExplorer)](https://github.com/Tymex321/FulcrumFileExplorer/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## Overview

Fulcrum replaces the everyday parts of Windows File Explorer (browsing, searching, copying
and organising files) with an app built for speed.

Its main feature is **instant search across the whole drive**. Windows Explorer searches
by walking through folders one at a time. Fulcrum reads the drive's file table (the NTFS
Master File Table) once and then keeps it current from the change journal Windows already
maintains. Filename search across an entire drive then returns results as you type.

## Features

- **Instant whole-drive search.** Optional. Turn it on once and approve one administrator
  prompt, and search covers every file on your NTFS drives and stays current as files
  change. Without it, search still works by scanning folders.
- **Flexible search:** plain text, fuzzy matching, wildcards (`*.pdf`) and regular
  expressions, with filters for type, extension, size and dates.
- **Fast browsing.** Very large folders show their first rows immediately while the rest
  load.
- **Tabs and dual pane:** multiple tabs, reopening closed tabs, and two panes side by side
  or stacked.
- **Four views:** details, list, tiles and grid, plus thumbnails and a preview pane.
- **File operations:** copy, move, rename, send to Recycle Bin and permanent delete. Large
  operations show progress and a time estimate, can be paused or cancelled, and ask what
  to do about name conflicts (skip, overwrite, keep both or overwrite if newer).
- **Drag and drop** and cut/copy/paste between folders, tabs and panes.
- **Quick access and pinned folders** in the sidebar.
- **Keyboard first:** a command palette (`Ctrl+Shift+P`), "go to file" (`Ctrl+P`) and
  shortcuts you can customise.
- **Personal touches:** accent colours, compact or comfortable density, font size, and
  restoring your tabs when you reopen the app.

## Screenshots

Screenshots will be added with the next release.

## Installation

1. Open the [latest release](https://github.com/Tymex321/FulcrumFileExplorer/releases/latest).
2. Download **`Fulcrum-Setup-<version>-x64.exe`**.
3. Run it and follow the installer.

Fulcrum then appears in the Start menu.

> **"Windows protected your PC"?** The installer is not code-signed yet, so Microsoft
> SmartScreen may warn about it. Click **More info**, then **Run anyway**.

### Updating

Fulcrum updates itself. A few seconds after it starts it checks GitHub for a newer version,
and if there is one a card appears in the sidebar. Click **Update now** and Fulcrum
downloads it, verifies it, installs it and restarts, keeping your settings and tabs. Windows
asks for administrator approval, as it did for the first install.

You can check at any time under **Settings → Updates**, or turn the startup check off there.
The check only asks GitHub for a small version file; nothing is installed unless you click
**Update now**. Updates are signed, and Fulcrum refuses any installer that is not signed with
the project's key.

To uninstall, use **Settings → Apps → Installed apps → Fulcrum → Uninstall**. This also
removes the instant-search service if you turned it on.

## Requirements

### For users

- Windows 10 or Windows 11, 64-bit.
- Microsoft Edge WebView2 Runtime. It is built into Windows 11 and current Windows 10, and
  the installer adds it automatically if it is missing.
- Administrator approval once to install, and once more if you turn on instant search.

Nothing else is needed.

### For developers

| Tool | Version | Notes |
|---|---|---|
| [Rust](https://rustup.rs) | stable, 1.90 or newer | `rust-toolchain.toml` selects the toolchain and components automatically |
| [Node.js](https://nodejs.org) | 22 or newer | includes npm, which builds the user interface |
| Tauri CLI | 2.x | `cargo install tauri-cli --version "^2" --locked` |
| [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) | current | Windows only: select **Desktop development with C++** |
| WebView2 Runtime | any | Windows only: preinstalled on Windows 11 |

The installer is built with NSIS, which the Tauri CLI downloads on first use.

## Building from source

Fulcrum's installer is built on Windows. From a clean checkout:

```powershell
git clone https://github.com/Tymex321/FulcrumFileExplorer.git
cd FulcrumFileExplorer
cargo install tauri-cli --version "^2" --locked
npm ci --prefix ui
cargo tauri build --config src-tauri/tauri.release.conf.json
```

The installer is written to
`target\release\bundle\nsis\Fulcrum_<version>_x64-setup.exe`.

Run every `cargo tauri` command from the repository root. The release config
(`tauri.release.conf.json`) also builds the instant-search service (`fx-helper.exe`) and
packages it with the app. This produces a working installer; it is not signed for updates
(see [Releasing](#releasing)), so only official releases update installed copies.

### Development

The whole project also builds and runs on macOS and Linux, where the Windows-specific parts
are replaced by portable versions. Day-to-day work does not need a Windows machine.

```bash
npm ci --prefix ui
cargo tauri dev                 # run the app with live reload
npm --prefix ui run dev         # or just the interface, in a browser, with sample data
```

Checks:

```bash
./scripts/check-all.sh          # formatting, lints, Rust tests, Windows typecheck, UI build and tests
cargo test --workspace          # Rust tests only
npm --prefix ui test            # UI tests only
cargo bench -p fx-index         # search benchmarks on a synthetic 1M-file index
```

The TypeScript types in `ui/src/bindings/` are generated from the Rust types. Every build
regenerates them, and `scripts/gen-bindings.sh` does it on its own.

## Releasing

Releases are built by the **Release** workflow when a `v*` tag is pushed. It builds the
installer on Windows, signs it for the updater, checks the signature against the key built
into the app, and publishes the installer and `latest.json` (the file installed copies read
to find updates) on the GitHub release.

One-time setup:

1. Generate a signing key pair. The private key never goes in the repository:
   ```bash
   cargo tauri signer generate -w ~/.fulcrum-signing/fulcrum-updater.key
   ```
2. Put the **public** key (the `.key.pub` file's contents) in `src-tauri/tauri.conf.json`
   under `plugins.updater.pubkey`.
3. In GitHub, open **Settings → Secrets and variables → Actions** and add the repository
   secret `TAURI_SIGNING_PRIVATE_KEY` (the private key file's contents), plus
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` if you set a password.

> **Keep the private key backed up.** Installed copies only trust updates signed with it. If
> it is lost, existing installs cannot be updated and users would have to reinstall by hand.

To release:

1. Set the new version in `Cargo.toml` (`[workspace.package]`), `src-tauri/tauri.conf.json`,
   and `ui/package.json` and `ui/package-lock.json`, and add a `## [x.y.z]` section to
   `CHANGELOG.md`. The workflow checks that the tag, the version and the changelog agree.
2. Commit, wait for CI to pass, then tag and push: `git tag -a vx.y.z -m "Fulcrum x.y.z"` and
   `git push origin vx.y.z`.

## Project structure

```
crates/
  fx-core/      shared types and the two backend interfaces (index and filesystem)
  fx-index/     the search index: MFT reader, change-journal updates, query engine
  fx-fs/        listing, copy/move/delete, thumbnails and icons
  fx-helper/    the optional Windows service that reads the drive for instant search
src-tauri/      the desktop app: window, commands, settings, installer configuration
ui/             the interface: React, TypeScript, Vite and Tailwind CSS
scripts/        development and CI helper scripts, and the release/update tooling
.github/        CI, release workflow and Dependabot configuration
```

### How privileges work

Reading a drive's file table requires administrator rights, so Fulcrum keeps that access in
one small, separate service (`fx-helper`). The app itself always runs as a normal user.
When you turn on instant search, Windows asks for approval once to install the service. The
app then asks it read-only search questions. The service never changes files.

## License

[MIT](LICENSE) © 2026 Tymex321
