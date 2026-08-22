#!/usr/bin/env node

// tauri-action assembles latest.json by reading the release asset, merging its
// own platform entries and re-uploading it. Concurrent matrix jobs therefore
// clobber each other: a job that reads before a sibling's upload lands drops
// that sibling's platforms. Rebuilding the manifest once, after every build has
// finished, is order-independent.

import { readFile, readdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import process from "node:process";

const [tag, version, directory = "release-assets"] = process.argv.slice(2);
const repository = process.env.GITHUB_REPOSITORY;
const token = process.env.GH_TOKEN ?? process.env.GITHUB_TOKEN;
const apiBase = process.env.GITHUB_API_URL ?? "https://api.github.com";

if (!tag || !version) {
  throw new Error("Usage: node scripts/rebuild-updater-manifest.mjs <tag> <version> [directory]");
}
if (!repository) throw new Error("GITHUB_REPOSITORY is required");
if (!token) throw new Error("GH_TOKEN is required");

// Ordered longest-first so `.AppImage.tar.gz` wins over `.tar.gz`.
const BUNDLES = [
  { suffix: ".AppImage.tar.gz", os: "linux", bundle: "appimage" },
  { suffix: ".AppImage", os: "linux", bundle: "appimage" },
  { suffix: ".app.tar.gz", os: "darwin", bundle: "app" },
  { suffix: ".deb", os: "linux", bundle: "deb" },
  { suffix: ".rpm", os: "linux", bundle: "rpm" },
  { suffix: ".exe", os: "windows", bundle: "nsis" },
  { suffix: ".msi", os: "windows", bundle: "msi" },
];

// Matches tauri-action's signature priority: the bundle the plain `{os}-{arch}`
// key points at, which is what older clients read.
const PREFERRED = { linux: ["appimage"], windows: ["nsis", "msi"], darwin: ["app"] };

const ARCHITECTURES = new Map([
  ["x86_64", "x86_64"],
  ["amd64", "x86_64"],
  ["x64", "x86_64"],
  ["aarch64", "aarch64"],
  ["arm64", "aarch64"],
  ["armv7", "armv7"],
  ["armhf", "armv7"],
  ["i686", "i686"],
  ["i386", "i686"],
  ["x86", "i686"],
]);

// The release is still a draft when this runs, and `GET /releases/tags/{tag}`
// returns 404 for drafts, so the release has to be resolved from the list.
const request = async (path) => {
  const response = await fetch(`${apiBase}${path}`, {
    headers: {
      Accept: "application/vnd.github+json",
      Authorization: `Bearer ${token}`,
      "User-Agent": "luma-updater-manifest-rebuilder",
    },
  });
  if (!response.ok) {
    throw new Error(`GitHub API request to ${path} failed: ${response.status} ${response.statusText}`);
  }
  return response.json();
};

let release;
for (let page = 1; !release; page += 1) {
  const releases = await request(`/repos/${repository}/releases?per_page=100&page=${page}`);
  release = releases.find((candidate) => candidate.tag_name === tag);
  if (releases.length < 100) break;
}
if (!release) throw new Error(`Release ${tag} was not found in ${repository}`);

const assets = [];
for (let page = 1; ; page += 1) {
  const batch = await request(`/repos/${repository}/releases/${release.id}/assets?per_page=100&page=${page}`);
  assets.push(...batch);
  if (batch.length < 100) break;
}

const assetIds = new Map(assets.map((asset) => [asset.name, asset.id]));

const classify = (name) => {
  const match = BUNDLES.find((candidate) => name.endsWith(candidate.suffix));
  if (!match) return undefined;
  for (const [token, arch] of ARCHITECTURES) {
    if (new RegExp(`(?<![0-9A-Za-z])${token}(?![0-9A-Za-z])`).test(name)) {
      return { os: match.os, bundle: match.bundle, arch };
    }
  }
  throw new Error(`Could not determine the architecture of ${name}`);
};

const platforms = {};
const seen = new Map();

for (const file of (await readdir(directory)).sort()) {
  if (!file.endsWith(".sig")) continue;

  const artifact = file.slice(0, -".sig".length);
  const classification = classify(artifact);
  if (!classification) continue;

  // The Tauri CLI signs bundles that are not always uploaded (notably
  // `.AppImage.tar.gz`). Those cannot be referenced by the manifest at all, and
  // the coverage check below still fails if skipping one leaves a gap.
  const id = assetIds.get(artifact);
  if (id === undefined) {
    console.warn(`Skipping ${file}: ${artifact} was not uploaded to ${tag}`);
    continue;
  }

  const { os, arch, bundle } = classification;
  const entry = {
    signature: await readFile(resolve(directory, file), "utf8"),
    url: `${apiBase}/repos/${repository}/releases/assets/${id}`,
  };

  platforms[`${os}-${arch}-${bundle}`] = entry;

  const preference = PREFERRED[os].indexOf(bundle);
  const target = `${os}-${arch}`;
  if (preference !== -1 && (seen.get(target) ?? Number.MAX_SAFE_INTEGER) > preference) {
    seen.set(target, preference);
    platforms[target] = entry;
  }
}

const missing = ["windows", "darwin", "linux"].filter(
  (os) => !Object.keys(platforms).some((target) => target.startsWith(`${os}-`)),
);
if (missing.length > 0) {
  throw new Error(
    `Rebuilt manifest is missing a target for: ${missing.join(", ")}. ` +
      `Signed artifacts present: ${[...assetIds.keys()].filter((name) => name.endsWith(".sig")).join(", ") || "none"}`,
  );
}

const manifest = {
  version,
  notes: "",
  pub_date: new Date().toISOString(),
  platforms,
};

await writeFile(resolve(directory, "latest.json"), `${JSON.stringify(manifest, null, 2)}\n`);

console.log(`Rebuilt latest.json for ${version} with ${Object.keys(platforms).length} targets:`);
for (const target of Object.keys(platforms).sort()) console.log(`  ${target}`);
