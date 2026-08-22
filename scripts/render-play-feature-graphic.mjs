/*
 * Google Play feature graphic: 1024 x 500 PNG.
 *
 * Composed from the assets that already define the product's identity — the
 * app icon's star glyph and gradient (branding/icon-composer/) and the marketing
 * site's palette (apps/website/src/styles.css) — so the listing banner, the site
 * and the launcher icon read as one thing.
 *
 * Rendered through the Playwright Chromium the screenshot scripts already use,
 * rather than adding an image-processing dependency.
 *
 * Run: pnpm screenshots:play:feature
 */
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { mkdir, readFile } from "node:fs/promises";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const desktopRoot = resolve(repoRoot, "apps", "desktop");

/* playwright is a devDependency of apps/desktop, not of the workspace root, and
 * pnpm does not hoist it — so a bare import here resolves from scripts/ and
 * fails whatever the cwd. Anchor resolution at the package that declares it. */
const fromDesktop = createRequire(resolve(desktopRoot, "package.json"));
const { chromium } = fromDesktop("playwright");

const WIDTH = 1024;
const HEIGHT = 500;

/* Play crops the feature graphic on some surfaces, and overlays a play button on
 * others. Keeping the logo and wordmark inside this inset stops either from
 * clipping the thing the banner exists to show. */
const SAFE_INSET = 72;

const starSvg = await readFile(
  resolve(repoRoot, "branding", "icon-composer", "star.svg"),
  "utf8",
);
/* Inlined as a data URI: a file:// <img> would need the page to be served from
 * the repo root, and the SVG is small enough that this stays readable. */
const starUri = `data:image/svg+xml;base64,${Buffer.from(starSvg).toString("base64")}`;

const html = `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  html, body {
    width: ${WIDTH}px;
    height: ${HEIGHT}px;
    overflow: hidden;
  }
  body {
    /* Website tokens: --background #08060f, --accent #f0ccfb, --muted #b4acc8. */
    background: #08060f;
    color: #f4f4f5;
    font-family: "Inter", "Segoe UI Variable", "Segoe UI", system-ui, sans-serif;
    -webkit-font-smoothing: antialiased;
    position: relative;
    /* Centred as one lockup. Play crops this graphic from the edges on some
       surfaces, so weight sits in the middle rather than against one side. */
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 44px;
    padding: 0 ${SAFE_INSET}px;
  }
  /* The icon's own purple glow (#7c6cf2), behind the mark so the banner picks
     up the same light the launcher icon does. */
  .glow {
    position: absolute;
    left: 50%;
    top: 50%;
    width: 900px;
    height: 620px;
    transform: translate(-50%, -50%);
    background: radial-gradient(ellipse at center, rgba(124,108,242,0.30) 0%, rgba(124,108,242,0.10) 48%, transparent 72%);
    pointer-events: none;
  }
  .mark {
    width: 232px;
    height: 232px;
    flex: none;
    position: relative;
  }
  .copy { position: relative; }
  .wordmark {
    font-size: 104px;
    font-weight: 700;
    letter-spacing: -0.035em;
    line-height: 1;
  }
  .tagline {
    margin-top: 16px;
    font-size: 33px;
    font-weight: 500;
    line-height: 1.2;
    letter-spacing: -0.01em;
    color: #f0ccfb;
  }
  .platforms {
    margin-top: 20px;
    font-size: 21px;
    letter-spacing: 0.01em;
    color: #b4acc8;
  }
</style>
</head>
<body>
  <div class="glow"></div>
  <img class="mark" src="${starUri}" alt="">
  <div class="copy">
    <div class="wordmark">Luma</div>
    <div class="tagline">Terminal, SSH &amp; SFTP client</div>
    <div class="platforms">Open source &middot; No account required</div>
  </div>
</body>
</html>`;

const outDir = resolve(repoRoot, "branding", "screenshots", "play");
await mkdir(outDir, { recursive: true });
const path = resolve(outDir, "feature-graphic.png");

const browser = await chromium.launch();
try {
  const page = await browser.newPage({
    viewport: { width: WIDTH, height: HEIGHT },
    deviceScaleFactor: 1,
    reducedMotion: "reduce",
  });
  await page.setContent(html, { waitUntil: "load" });
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({ path, animations: "disabled" });
} finally {
  await browser.close();
}

const buffer = await readFile(path);
const [width, height] = [buffer.readUInt32BE(16), buffer.readUInt32BE(20)];
const ok = width === WIDTH && height === HEIGHT;
console.log(
  `[feature-graphic] ${width}x${height}` +
    `${ok ? "" : `  !! expected ${WIDTH}x${HEIGHT}`}, ${(buffer.length / 1024).toFixed(0)} KiB -> ${path}`,
);
if (!ok) process.exitCode = 1;
