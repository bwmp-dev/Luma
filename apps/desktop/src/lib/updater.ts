import { Channel, invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { getVersion } from "@tauri-apps/api/app";

/**
 * Thin wrappers around the native updater commands. All calls can reject in
 * local/dev builds because `tauri.conf.json` ships a placeholder updater key
 * (release CI injects the real value). Callers MUST treat any rejection as a
 * non-fatal "couldn't check" and never block startup on it.
 */

/** Update metadata surfaced to the UI once an update is found. */
export type UpdateInfo = {
  /** The version available to install. */
  version: string;
  /** The version currently running. */
  currentVersion: string;
  /** Release notes / changelog body, when the manifest provides one. */
  notes: string | null;
};

export type UpdateChannel = "stable" | "nightly";

export type DownloadEvent =
  | { event: "Started"; data: { contentLength?: number | null } }
  | { event: "Progress"; data: { chunkLength: number } }
  | { event: "Finished" };

export type UpdateHandle = {
  downloadAndInstall: (onEvent: (event: DownloadEvent) => void) => Promise<void>;
};

/** A found update plus the handle used to download and install it. */
export type FoundUpdate = {
  update: UpdateHandle;
  info: UpdateInfo;
};

/**
 * Ask the selected channel's update endpoint whether a newer version exists.
 * Resolves to the update handle + metadata, or `null` when up to date.
 * Rejects when the endpoint/pubkey is unreachable or invalid (dev builds).
 */
export async function checkForUpdate(
  channel: UpdateChannel,
): Promise<FoundUpdate | null> {
  const info = await invoke<UpdateInfo | null>("updater_check", { channel });
  if (!info) return null;
  return {
    info: { ...info, notes: info.notes?.trim() ? info.notes.trim() : null },
    update: {
      downloadAndInstall: async (onEvent) => {
        const onEventChannel = new Channel<DownloadEvent>();
        onEventChannel.onmessage = onEvent;
        await invoke("updater_download_and_install", {
          onEvent: onEventChannel,
        });
      },
    },
  };
}

/** Current app version from the Tauri runtime (best-effort). */
export { getVersion };

/**
 * Restart the app to finish applying an installed update. Authorized by the
 * `process:allow-restart` capability. Rejects when the runtime can't restart
 * (e.g. missing capability, non-Tauri context); callers MUST treat a rejection
 * as non-fatal and fall back to asking the user to restart manually. Never
 * exits the process — only relaunches.
 */
export async function relaunchApp(): Promise<void> {
  await relaunch();
}

/** Compact human-readable byte size for download progress. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${Math.round(kb)} KB`;
  const mb = kb / 1024;
  return `${mb.toFixed(1)} MB`;
}
