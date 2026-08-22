# Termius migration tools

`Export-TermiusVault.ps1` creates a read-only snapshot of the IndexedDB stores
used by Termius Desktop on Windows. It does not stop Termius, accept a vault
password, modify the Termius profile, or print record values.

Close Termius completely, then run:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\Export-TermiusVault.ps1
```

Unlock Termius in the window opened by the script and return to PowerShell when
prompted. The resulting `termius-vault-export.json` is restricted to the current
Windows account. Treat it as a secret: depending on the Termius vault mode, it
can contain encrypted or locally available credential and private-key material.

The snapshot is an intermediate migration bundle. Import into Luma should only
be performed after the bundle passes schema validation and an item-count preview.

## Performance benchmark

Build Luma, then run the dependency-free Node benchmark:

```sh
pnpm tauri build
node scripts/benchmark.mjs
```

You can pass a binary path directly or set `LUMA_BENCH_BINARY` when the default
`apps/desktop/src-tauri/target/release` location is not appropriate:

```sh
node scripts/benchmark.mjs /path/to/luma
```

The script times process launch through an explicit ready signal, an OS-visible
window, or the configured timeout. It samples resident set size (RSS) with
`Get-Process` on Windows or `ps` on macOS/Linux, prints JSON plus a summary table,
and writes timestamped JSON to `scripts/benchmark-results/`.

For instrumented/headless startup measurements, set `LUMA_BENCH_READY_FILE` to a
path that the running app or test harness creates when initialization is complete.
Timeout and sampling durations can be adjusted with
`LUMA_BENCH_STARTUP_TIMEOUT_MS`, `LUMA_BENCH_IDLE_SAMPLE_MS`, and
`LUMA_BENCH_SAMPLE_INTERVAL_MS`.

Metrics that require UI automation or a live terminal/SFTP workload are not
fabricated. Each JSON report includes manual procedures for memory per terminal,
CPU during high output, memory after opening/closing 20 sessions, large
scrollback, and SFTP transfer memory.

Updater release-key setup and release artifact details are summarized in the
[release section](../README.md#releases).

## App Store screenshots (iOS Simulator)

`capture-simulator-screenshots.mjs` photographs the real app running in a
simulator, at exactly the pixel sizes App Store Connect accepts:

| Simulator      | Output      | Slot          |
| -------------- | ----------- | ------------- |
| `Luma-iPhone-6.5` (iPhone 14 Plus)   | 1284 × 2778 | iPhone 6.5"  |
| `Luma-iPad-13` (iPad Pro 13-inch M4) | 2064 × 2752 | iPad 13"     |

Create those once:

```sh
xcrun simctl create "Luma-iPhone-6.5" com.apple.CoreSimulator.SimDeviceType.iPhone-14-Plus com.apple.CoreSimulator.SimRuntime.iOS-26-5
xcrun simctl create "Luma-iPad-13" com.apple.CoreSimulator.SimDeviceType.iPad-Pro-13-inch-M4-8GB com.apple.CoreSimulator.SimRuntime.iOS-26-5
```

The older `screenshots:ios` / `screenshots:ipad` scripts render the showcase in
headless Chromium. That cannot photograph the Liquid Glass tab bar, the native
context menus or the keyboard accessory — `useNativeTabBar` treats a missing
plugin as "not iOS" and quietly renders the web fallback instead. So this script
runs the app for real and only mocks the data layer.

Three terminals:

```sh
pnpm showcase:serve          # showcase bundle + scenario channel on :4173
pnpm showcase:sim:iphone     # boots the simulator, builds and installs the app
pnpm screenshots:appstore:iphone
```

Output lands in `branding/screenshots/appstore/<device>/<theme>/`.

The marketing site's mobile carousel is derived from the same PNGs rather than
captured separately, so the website shows the shipping native UI too:

```sh
pnpm screenshots:website-mobile   # downscales 1284 x 2778 -> 428 x 926 and @2x
```

## Play Store assets (Android)

Android does not need a device or a simulator: the mobile shell is pure DOM
there. The Liquid Glass tab bar is iOS-only — `attachNativeTabBar` returns false
for any non-iOS platform — so the web capsule Chromium draws is exactly what the
Android build ships, and the headless capture is faithful.

```sh
pnpm screenshots:play             # phone + both tablet slots, dark and light
pnpm screenshots:play --device phone
pnpm screenshots:play:feature     # 1024 x 500 feature graphic
```

Output lands in `branding/screenshots/play/<slot>/<theme>/` plus
`branding/screenshots/play/feature-graphic.png`.

Play accepts 16:9 or 9:16 only, so every geometry divides exactly into 9:16 and
sits inside the bounds for its slot:

| Slot                | Pixels      | Play requirement          |
| ------------------- | ----------- | ------------------------- |
| `phone`             | 1080 x 1920 | sides 320..3840, 2-8 shots |
| `tablet-7`          | 1152 x 2048 | sides 320..3840           |
| `tablet-10`         | 1440 x 2560 | sides 1080..7680          |
| feature graphic     | 1024 x 500  | exact, PNG/JPEG, <= 15 MB  |

Two things the script has to do that are easy to miss:

- **Theme comes from `prefers-color-scheme`, not the seeded setting.**
  `hooks/useTheme` resolves the appearance from the media query, so the capture
  calls `page.emulateMedia({ colorScheme })`. Without it every "dark" shot comes
  back in Chromium's default light appearance — and it still reports success.
- **The viewport width picks the terminal seed.** At or under 600 CSS px the
  harness seeds the short-line session; above it, the full-width fetch output
  (`isNarrowViewport` in `src/showcase/invokeHandlers.ts`). 7-inch is captured at
  576 CSS px so it stays on the narrow seed — at 603 the wide output wrapped.

The scripts resolve `vite` and `playwright` through `createRequire` anchored at
`apps/desktop/package.json`. Both are devDependencies of that package, not of the
workspace root, and pnpm does not hoist them, so a bare `import` would fail from
`scripts/` whatever the cwd.

Notes:

- `devUrl` must be a **bare origin**. Tauri appends request paths to it as a
  string, so `http://host/showcase.html?x=1` makes the app fetch
  `/showcase.html?x=1/src/showcase/main.tsx`. The dev server therefore serves
  the showcase at `/` and takes its boot values from `SHOWCASE_*` env vars.
- Scenes are driven over `/__showcase/scenario`; the page reports back on
  `/__showcase/ready`, so captures wait for the UI instead of sleeping.
- The page relays JS errors to `/__showcase/log`, which is the only console you
  get from the app's webview.
- The script cold-restarts the app before capturing: the scenario watcher is a
  loop started at boot, so an HMR edit alone would leave it running old code.
