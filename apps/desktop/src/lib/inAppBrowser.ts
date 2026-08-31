import { invoke } from "@tauri-apps/api/core";

/*
 * Browser hosted inside the app (iOS: SFSafariViewController, via
 * src-tauri/src/commands/in_app_browser.rs).
 *
 * It exists for one kind of URL: one this app is serving. A web preview is a
 * loopback port forwarded by a tunnel inside Luma, so sending the system browser
 * to it backgrounds Luma — and iOS suspends a backgrounded app within seconds,
 * which stops the tunnel answering and leaves the page hanging half-loaded. A
 * browser the app presents keeps the app foreground-active, so the tunnel keeps
 * running for as long as the page is open.
 */

/**
 * Open `url` in the in-app browser.
 *
 * Resolves false wherever there is no such browser — Android, desktop, tests —
 * which is the caller's cue to fall back to the system handler. Nothing is lost
 * by that fallback: no other platform suspends the app serving the tunnel.
 */
export async function openInAppBrowser(url: string): Promise<boolean> {
  try {
    await invoke("browser_open_in_app", { url });
    return true;
  } catch {
    return false;
  }
}
