import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const json = (path) => JSON.parse(readFileSync(resolve(root, path), "utf8"));
const tauri = json("apps/desktop/src-tauri/tauri.conf.json");
const desktop = json("apps/desktop/package.json");
const manifest = json(".release-please-manifest.json");
const cargo = readFileSync(resolve(root, "apps/desktop/src-tauri/Cargo.toml"), "utf8")
  .match(/^version\s*=\s*"([^"]+)"/m)?.[1];

const versions = new Map([
  ["Tauri", tauri.version],
  ["desktop package", desktop.version],
  ["Cargo package", cargo],
  ["Release Please manifest", manifest["."]],
]);
const expected = tauri.version;
const errors = [];
for (const [name, version] of versions) {
  if (version !== expected) errors.push(`${name} is ${version ?? "missing"}; expected ${expected}`);
}

const status = execFileSync("git", ["status", "--porcelain"], {
  cwd: root,
  encoding: "utf8",
}).trim();
if (status && !process.argv.includes("--allow-dirty")) {
  errors.push("the working tree is not clean (pass --allow-dirty for a development preflight)");
}

const requiredTools = ["node", "pnpm", "cargo", "git"];
for (const tool of requiredTools) {
  try {
    execFileSync(process.platform === "win32" ? "where.exe" : "which", [tool], {
      cwd: root,
      stdio: "ignore",
    });
  } catch {
    errors.push(`${tool} is not available on PATH`);
  }
}

console.log(`Luma ${expected} release preflight`);
for (const [name, version] of versions) console.log(`  ${name}: ${version}`);
console.log(`  working tree: ${status ? "dirty" : "clean"}`);
console.log(`  updater endpoint: ${tauri.plugins?.updater?.endpoints?.[0] ?? "missing"}`);

if (errors.length) {
  for (const error of errors) console.error(`ERROR: ${error}`);
  process.exitCode = 1;
} else {
  console.log("Release metadata and local tooling are ready.");
}
