#!/usr/bin/env node
// Checks that an installer's `.sig` verifies against the public key built into the app.
//
//   node scripts/verify-update-signature.mjs <installer> <signature-file> <tauri.conf.json>
//
// Releases are signed with a private key kept in a repository secret, and installed copies
// verify updates against `plugins.updater.pubkey`. If the secret is the wrong key, the
// release would publish normally and then every update would be silently rejected on
// users' machines. Running this before publishing turns that into a failed build.
//
// Tauri signs with minisign: Ed25519, over the BLAKE2b-512 hash of the file ("ED" mode).
import { createHash, createPublicKey, verify } from "node:crypto";
import { readFileSync } from "node:fs";

const [installer, sigFile, confFile] = process.argv.slice(2);
if (!installer || !sigFile || !confFile) {
  console.error("usage: verify-update-signature.mjs <installer> <signature-file> <tauri.conf.json>");
  process.exit(2);
}

const fail = (msg) => {
  console.error(`signature check failed: ${msg}`);
  process.exit(1);
};

// Both the key and the signature are base64 of a two-line minisign file.
const decodeMinisign = (b64) => Buffer.from(b64.trim(), "base64").toString("utf8").split("\n");
const payload = (b64) => Buffer.from(decodeMinisign(b64)[1].trim(), "base64");

const conf = JSON.parse(readFileSync(confFile, "utf8"));
const pubkeyB64 = conf?.plugins?.updater?.pubkey;
if (!pubkeyB64) fail(`${confFile} has no plugins.updater.pubkey`);

const key = payload(pubkeyB64); // alg(2) | key id(8) | ed25519 public key(32)
const sig = payload(readFileSync(sigFile, "utf8")); // alg(2) | key id(8) | signature(64)
if (key.length !== 42 || sig.length !== 74) fail("unexpected key or signature length");
if (!key.subarray(2, 10).equals(sig.subarray(2, 10))) {
  fail("the installer was signed with a different key than the one built into the app");
}

const algorithm = sig.subarray(0, 2).toString("latin1");
const data = readFileSync(installer);
const message = algorithm === "ED" ? createHash("blake2b512").update(data).digest() : data;

// Node wants a SubjectPublicKeyInfo wrapper around the raw 32-byte Ed25519 key.
const spki = Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), key.subarray(10)]);
const ok = verify(null, message, createPublicKey({ key: spki, format: "der", type: "spki" }), sig.subarray(10));
if (!ok) fail("the signature does not match the installer's contents");

console.log(`signature ok (${installer})`);
