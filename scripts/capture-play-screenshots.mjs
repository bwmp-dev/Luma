/*
 * Google Play Store screenshots for the Android build.
 *
 * Unlike the App Store captures (capture-simulator-screenshots.mjs), this runs
 * the showcase bundle in headless Chromium rather than a device. That is
 * faithful here: on Android the mobile shell is pure DOM. The native Liquid
 * Glass tab bar is iOS-only — `attachNativeTabBar` returns false for any non-iOS
 * platform, so Android ships the same web capsule Chromium draws. Nothing in
 * these shots is a fallback the app does not use.
 *
 * Play accepts 16:9 or 9:16 only, so every geometry below divides exactly into
 * 9:16 and each is inside the size bounds for its slot:
 *
 *   phone     1080 x 1920   sides 320..3840
 *   tablet-7  1152 x 2048   sides 320..3840
 *   tablet-10 1440 x 2560   sides 1080..7680
 *
 * Run: pnpm screenshots:play          (all three slots)
 *      pnpm screenshots:play --device phone
 */
import { fileURLToPath, pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { mkdir, rm, readFile } from "node:fs/promises";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const desktopRoot = resolve(repoRoot, "apps", "desktop");
const configFile = resolve(desktopRoot, "showcase.vite.config.ts");

/* vite and playwright are devDependencies of apps/desktop, not of the workspace
 * root, and pnpm does not hoist them — so a bare import here resolves from
 * scripts/ and fails whatever the cwd. Anchor resolution at the package that
 * declares them. */
const fromDesktop = createRequire(resolve(desktopRoot, "package.json"));
// vite is ESM, playwright is CJS — each has to be loaded the way it ships.
const { build, preview } = await import(
  pathToFileURL(fromDesktop.resolve("vite")).href
);
const { chromium } = fromDesktop("playwright");

const THEMES = ["dark", "light"];

/* The order Play Console will list them in. Each is a route in the mobile
 * shell; see apps/desktop/src/showcase/scenarios.ts for what each sets up. */
const SCENES = [
  { view: "terminal", name: "01-terminal" },
  { view: "servers", name: "02-server-dashboard" },
  { view: "hosts", name: "03-hosts" },
  { view: "vaults", name: "04-vaults" },
  { view: "sftp", name: "05-sftp" },
  { view: "agent-inbox", name: "06-agent-inbox" },
];

/* CSS size x deviceScaleFactor must land exactly on the pixel size, or Play
 * rejects the ratio.
 *
 * The widths also decide which terminal session the harness seeds: at or below
 * 600 CSS px it uses the short-line one, above that the full-width fetch output
 * (see isNarrowViewport in src/showcase/invokeHandlers.ts). 576 is a realistic
 * 7-inch portrait width AND stays under that cutoff, so the terminal reads
 * cleanly; 720 is wide enough for the full-width output not to wrap. */
const DEVICES = {
  phone: { css: [432, 768], scale: 2.5, expect: [1080, 1920], outDir: "phone" },
  "tablet-7": { css: [576, 1024], scale: 2, expect: [1152, 2048], outDir: "tablet-7" },
  "tablet-10": { css: [720, 1280], scale: 2, expect: [1440, 2560], outDir: "tablet-10" },
};

function parseArgs() {
  const args = process.argv.slice(2);
  const index = args.indexOf("--device");
  if (index < 0) return Object.keys(DEVICES);
  const key = args[index + 1];
  if (!DEVICES[key]) {
    throw new Error(`unknown --device ${key}; expected one of ${Object.keys(DEVICES).join(", ")}`);
  }
  return [key];
}

const selected = parseArgs();

await build({ configFile, root: desktopRoot, logLevel: "warn" });
const server = await preview({ configFile, root: desktopRoot, logLevel: "warn" });
const base =
  server.resolvedUrls?.local?.[0] ?? `http://localhost:${server.config.preview.port}/`;
const browser = await chromium.launch();

let failed = false;

try {
  for (const key of selected) {
    const device = DEVICES[key];
    const context = await browser.newContext({
      viewport: { width: device.css[0], height: device.css[1] },
      deviceScaleFactor: device.scale,
      isMobile: true,
      hasTouch: true,
      reducedMotion: "reduce",
    });
    const page = await context.newPage();
    page.setDefaultNavigationTimeout(90_000);
    page.on("pageerror", (error) => console.error(`[capture:play] page error: ${error.message}`));

    for (const theme of THEMES) {
      /* The app resolves dark/light from prefers-color-scheme (hooks/useTheme),
       * not from the seeded setting — so the media query has to be emulated or
       * every shot comes back in Chromium's default light appearance. This also
       * matches how the theme is actually chosen on a device. */
      await page.emulateMedia({ colorScheme: theme });

      const outDir = resolve(repoRoot, "branding", "screenshots", "play", device.outDir, theme);
      // Clear first: the scene list changes between releases, and a renamed or
      // dropped scene would otherwise leave a stale PNG in the upload folder.
      await rm(outDir, { recursive: true, force: true });
      await mkdir(outDir, { recursive: true });

      for (const scene of SCENES) {
        const path = resolve(outDir, `${scene.name}.png`);
        await page.goto(`${base}showcase.html?view=${scene.view}&theme=${theme}&platform=android`);
        await page.waitForSelector('html[data-showcase-ready="true"]', { timeout: 45_000 });
        if (scene.view === "terminal") {
          await page.waitForFunction(
            () => (document.querySelector(".xterm-rows")?.textContent ?? "").trim().length > 20,
          );
        }
        await page.waitForTimeout(250);
        await page.screenshot({ path, animations: "disabled" });

        const [width, height] = await pixelSize(path);
        const ok = width === device.expect[0] && height === device.expect[1];
        if (!ok) failed = true;
        console.log(
          `[capture:play] ${key}/${theme}/${scene.name} ${width}x${height}` +
            `${ok ? "" : `  !! expected ${device.expect.join("x")}`} -> ${path}`,
        );
      }
    }
    await context.close();
  }
} finally {
  await browser.close();
  await server.httpServer.close();
}

if (failed) process.exitCode = 1;

/** Read a PNG's dimensions from its IHDR, so a wrong-sized capture fails the
 * run here instead of at upload. */
async function pixelSize(path) {
  const buffer = await readFile(path);
  return [buffer.readUInt32BE(16), buffer.readUInt32BE(20)];
}
