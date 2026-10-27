import { useEffect, useState } from "react";
import type { Window as TauriWindow } from "@tauri-apps/api/window";
import { isMac } from "../lib/platform";

// Keep in sync with tauri.macos.conf.json. Wry adds y to the native title-bar
// container height, so it is not a CSS top inset; 22 aligns with our 40px bar.
export const TRAFFIC_LIGHT_POSITION = { x: 14, y: 22 };

/** macOS draws native traffic lights over the title bar, except in full screen
 * where it hides them, so the bar only needs to clear them while windowed. */
export function useTrafficLightInset(win: TauriWindow): boolean {
  const [fullscreen, setFullscreen] = useState(false);

  useEffect(() => {
    if (!isMac()) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const sync = () =>
      void win
        .isFullscreen()
        .then((value) => {
          if (!cancelled) setFullscreen(value);
        })
        .catch(() => {});

    sync();
    void win.onResized(sync).then((cleanup) => {
      if (cancelled) cleanup();
      else unlisten = cleanup;
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [win]);

  return isMac() && !fullscreen;
}
