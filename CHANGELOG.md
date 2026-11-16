# Changelog

All notable changes to Fulcrum are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [1.0.1] - 2026-11-16

### Added

- **In-app updates.** Fulcrum checks for a newer version a few seconds after it starts and
  offers it in the sidebar. **Update now** downloads the new version, checks its signature
  against the key built into the app, installs it and restarts Fulcrum. Your settings and
  tabs are kept. Use **Settings → Updates → Check for updates** (or the command palette) to
  check on demand, or turn the startup check off there.

### Changed

- **"Go to file" (Ctrl+P) shows only accurate matches.** It now looks for your text
  literally and lists those files, best match first: exact name, then names that start with
  your text, then word starts. If nothing matches literally it falls back to close
  (typo-tolerant) matches only, so scattered letters no longer pad the list with unrelated
  files. Previously the search could also list the *worst* matches first.

### Fixed

- Results that arrived after the search call returned were not drawn until you edited the
  query, leaving "No matches" next to a non-zero match count.
- Moving the window was slow and laggy because of a blur effect Windows re-renders on every
  frame; it is now off.

### Upgrading from 1.0.0

Version 1.0.0 has no updater, so install 1.0.1 once by hand. From then on Fulcrum updates
itself.

## [1.0.0] - 2026-11-16

The first public release of Fulcrum, a fast file explorer for Windows.

### Highlights

- **Instant whole-drive search.** Fulcrum reads the NTFS Master File Table and keeps it
  current from the USN change journal, so filename search across an entire drive returns
  results as you type. It is optional and needs one administrator approval to turn on.
  Without it, search falls back to scanning folders.
- **Search modes:** plain text, fuzzy, wildcards and regular expressions, with filters for
  type, extension, size and dates.
- **Browsing built for large folders:** listings appear immediately while they load.
- **Tabs and dual pane**, side by side or stacked, with reopening of closed tabs.
- **Details, list, tiles and grid views**, with thumbnails and a preview pane.
- **File operations** with real progress, time estimates, pause/cancel and conflict
  handling (skip, overwrite, keep both, overwrite if newer).
- **Drag and drop**, cut/copy/paste, Recycle Bin and permanent delete.
- **Quick access and pinned folders.**
- **Command palette, "go to file" and customisable keyboard shortcuts.**
- **Accent colours, density and font size**, and restoring your previous session.

### Installation

Download `Fulcrum-Setup-1.0.0-x64.exe` below and run it. Requires Windows 10 or 11
(64-bit). The installer adds the Microsoft Edge WebView2 Runtime if it is missing.

The installer is not code-signed yet, so Windows SmartScreen may show a warning. Click
**More info**, then **Run anyway**.

### Known limitations

- Instant search works on NTFS drives only. Other drives (FAT32, exFAT, network shares) use
  folder-scanning search.
- Turning on instant search needs administrator approval once. Upgrading the app keeps the
  service, and uninstalling the app removes it.
- The installer is not code-signed.
- Windows Explorer's own context-menu extensions (entries that other apps add to the
  right-click menu) are not shown yet.
