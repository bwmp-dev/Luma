import type { UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import {
  onTabSelected,
  setActiveTab,
  setBadge,
  setHidden,
  setItems,
} from "tauri-plugin-ios-glass-tabbar-api";
import { useMobileNavStore, type MobileTab } from "../../stores/mobileNavStore";
import { useCapabilityStore } from "../../stores/capabilityStore";
import { TAB_ITEMS } from "./MobileTabBar";

/*
 * Bridge to tauri-plugin-ios-glass-tabbar. The plugin installs a stock UITabBar
 * over the webview, which automatically adopts Liquid Glass when the app is
 * built with the iOS 26 SDK.
 */

/** Height used for the web capsule; the native bar overwrites this on attach. */
const WEB_TAB_BAR_HEIGHT = 68;

type NativeTabItem = {
  key: MobileTab;
  title: string;
  sfSymbol: string;
};

function setTabBarHeight(height: number): void {
  document.documentElement.style.setProperty(
    "--mobile-tabbar-height",
    `${height}px`,
  );
}

/** Whether the native bar successfully attached for this session. */
let nativeActive = false;

export function isNativeTabBarActive(): boolean {
  return nativeActive;
}

/**
 * Try to hand the tab bar over to the native plugin. Returns true when the
 * native bar is live (the React capsule must then not render). Safe to call on
 * any platform: a missing command or a non-iOS host resolves false.
 */
export async function attachNativeTabBar(
  sessionCount: number,
): Promise<boolean> {
  // Plugin calls deliberately no-op on Android, so a resolved setItems call is
  // not enough to prove a native bar exists there.
  if (useCapabilityStore.getState().capabilities.os !== "ios") {
    nativeActive = false;
    setTabBarHeight(WEB_TAB_BAR_HEIGHT);
    return false;
  }

  try {
    const selectedIndex = tabIndex(useMobileNavStore.getState().tab);
    // setItems is what decides whether a native bar exists: it adds the bar to
    // the window. Everything after it is decoration on a bar that is already on
    // screen, so it must not be able to fail the attach -- reporting "not
    // attached" once the view exists renders the web capsule underneath it.
    await setItems(nativeTabs(), selectedIndex);
    nativeActive = true;
    await Promise.all([syncBadges(sessionCount), syncTintColor()]);
    setTabBarHeight(WEB_TAB_BAR_HEIGHT);
    return true;
  } catch {
    // Android, iOS below the plugin's minimum, a denied command permission, or
    // a harness with no backend: fall back to the web capsule.
    nativeActive = false;
    setTabBarHeight(WEB_TAB_BAR_HEIGHT);
    return false;
  }
}

function nativeTabs(): NativeTabItem[] {
  return TAB_ITEMS.map((item) => ({
    key: item.tab,
    title: item.label,
    sfSymbol: item.sfSymbol,
  }));
}

function tabIndex(tab: MobileTab): number {
  return TAB_ITEMS.findIndex((item) => item.tab === tab);
}

/** The theme accent, flattened to #rrggbb.
 *
 * Themes may specify colors as #rrggbbaa and the accent is carried through
 * verbatim, but UIColor is built from a fixed-width hex string: an 8-digit
 * value used to be rejected outright, leaving the bar tinted by the previous
 * theme. Alpha is dropped rather than honoured -- a translucent tint on the
 * selected tab icon reads as a washed-out icon, not as a design choice.
 */
export function resolvedAccentColor(): string | null {
  const accent = getComputedStyle(document.documentElement)
    .getPropertyValue("--accent")
    .trim();
  const match = /^#([0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i.exec(accent);
  if (!match) return null;
  const hex = match[1];
  if (hex.length === 3) {
    return `#${hex[0]}${hex[0]}${hex[1]}${hex[1]}${hex[2]}${hex[2]}`;
  }
  return `#${hex.slice(0, 6)}`;
}

// Cosmetic: a tint the plugin will not accept must never fail the caller. The
// native bar is added to the window by setItems, so a throw here used to abort
// attach with the bar already on screen -- and the web capsule would then
// render underneath it, showing two tab bars at once.
async function syncTintColor(): Promise<void> {
  const color = resolvedAccentColor();
  if (!color) return;
  try {
    await invoke("plugin:ios-glass-tabbar|set_tint_color", {
      payload: { color },
    });
  } catch {
    // Leaves the previous tint in place; the next theme change retries.
  }
}

// Cosmetic, like the tint: never fail the caller once the bar exists.
async function syncBadges(sessionCount: number): Promise<void> {
  try {
    await setBadge(
      tabIndex("connections"),
      sessionCount > 0 ? String(sessionCount) : null,
    );
  } catch {
    // Stale badge until the next session-count change retries.
  }
}

/** Mirror the store's selected tab into the native bar. No-op when inactive. */
export async function syncNativeTabBar(
  tab: MobileTab,
  sessionCount: number,
): Promise<void> {
  if (!nativeActive) return;
  try {
    await Promise.all([
      setActiveTab(tabIndex(tab)),
      syncBadges(sessionCount),
      syncTintColor(),
    ]);
  } catch {
    // A failed mirror leaves the bar showing a stale selection for one frame;
    // not worth tearing the bar down over.
  }
}

/**
 * Show or hide the native bar. Hidden while a terminal session is full-screen
 * (the session owns the whole viewport) and while the keyboard is up.
 */
export async function setNativeTabBarVisible(visible: boolean): Promise<void> {
  if (!nativeActive) return;
  try {
    await setHidden(!visible);
  } catch {
    // Ignore: visibility is cosmetic, and the next state change retries.
  }
}

/** Subscribe to native tab selections, routing them into the nav store. */
export async function listenNativeTabBar(): Promise<UnlistenFn> {
  const listener = await onTabSelected(({ key }) => {
    if (key === "vaults" || key === "connections" || key === "profile") {
      useMobileNavStore.getState().selectTab(key);
    }
  });
  return () => listener.unregister();
}
