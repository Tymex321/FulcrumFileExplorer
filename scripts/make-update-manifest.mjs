#!/usr/bin/env node
// Writes the `latest.json` that installed copies of Fulcrum read to find updates.
//
//   node scripts/make-update-manifest.mjs <version> <installer-name> <signature-file> <notes-file> <repo>
//
// The app (src-tauri/tauri.conf.json, plugins.updater.endpoints) fetches
// https://github.com/<repo>/releases/latest/download/latest.json, compares `version` with
// its own, and downloads `url`. The signature is the contents of the `.sig` file Tauri
// writes next to the installer; the app refuses an installer that does not match it.
import { readFileSync } from "node:fs";

const [version, installer, sigFile, notesFile, repo] = process.argv.slice(2);
if (!version || !installer || !sigFile || !notesFile || !repo) {
  console.error("usage: make-update-manifest.mjs <version> <installer-name> <signature-file> <notes-file> <repo>");
  process.exit(2);
}

const signature = readFileSync(sigFile, "utf8").trim();
if (!signature) {
  console.error(`signature file ${sigFile} is empty`);
  process.exit(1);
}

const manifest = {
  version,
  notes: readFileSync(notesFile, "utf8").trim(),
  pub_date: new Date().toISOString(),
  platforms: {
    "windows-x86_64": {
      signature,
      url: `https://github.com/${repo}/releases/download/v${version}/${installer}`,
    },
  },
};

process.stdout.write(`${JSON.stringify(manifest, null, 2)}\n`);
