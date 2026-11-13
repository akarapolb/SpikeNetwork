#!/usr/bin/env node
// Writes latest.json, the static update manifest the desktop app reads (docs/desktop-release.md).
//
//   node scripts/make-update-manifest.mjs <artifacts-dir> <repo> <tag> <out-file> [notes-file]
//
// <artifacts-dir> holds the updater packages and their signatures as the release workflow names them:
//   spike-connect-<version>-windows-x64.exe      + .sig
//   spike-connect-<version>-linux-x86_64.AppImage + .sig
// The signature file's contents are copied into the manifest unchanged; the app checks them against the public
// key in tauri.conf.json before installing anything.
import { readFileSync, readdirSync, writeFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const [, , dir, repo, tag, out, notesFile] = process.argv;
if (!dir || !repo || !tag || !out) {
  console.error("usage: make-update-manifest.mjs <artifacts-dir> <owner/repo> <tag> <out-file> [notes-file]");
  process.exit(2);
}

const here = dirname(fileURLToPath(import.meta.url));
const conf = JSON.parse(readFileSync(join(here, "..", "desktop", "src-tauri", "tauri.conf.json"), "utf8"));
const version = conf.version;
if (tag !== `v${version}`) {
  console.error(`tag ${tag} does not match desktop version ${version} in tauri.conf.json`);
  process.exit(1);
}

const files = readdirSync(dir);
const platforms = {
  "windows-x86_64": `spike-connect-${version}-windows-x64.exe`,
  "linux-x86_64": `spike-connect-${version}-linux-x86_64.AppImage`,
};

const manifest = {
  version,
  notes: existsSync(notesFile || "") ? readFileSync(notesFile, "utf8").trim() : `Spike Connect ${version}`,
  pub_date: new Date().toISOString(),
  platforms: {},
};

for (const [target, name] of Object.entries(platforms)) {
  if (!files.includes(name)) {
    console.error(`missing ${name} in ${dir}`);
    process.exit(1);
  }
  const sigName = `${name}.sig`;
  if (!files.includes(sigName)) {
    console.error(`missing ${sigName}: the updater package was not signed (set TAURI_SIGNING_PRIVATE_KEY)`);
    process.exit(1);
  }
  manifest.platforms[target] = {
    signature: readFileSync(join(dir, sigName), "utf8").trim(),
    url: `https://github.com/${repo}/releases/download/${tag}/${name}`,
  };
}

writeFileSync(out, JSON.stringify(manifest, null, 2) + "\n");
console.log(`wrote ${out} for ${Object.keys(manifest.platforms).join(", ")}`);
