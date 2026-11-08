import { create } from "zustand";
import type { SidebarSection } from "../types";
import type { VaultJoinLink } from "../lib/vaults";

/**
 * What the main area shows. Decoupled from the sidebar rail: `"terminal"` is
 * the tab area driven by the top TabBar (a terminal tab or a page tab), the rail
 * selects a section screen, and keychain and the other tools are full-screen
 * views. This is a single source of truth so a tab can be the active main view
 * independently of any sidebar section.
 */
export type MainView =
  | SidebarSection
  | "terminal"
  | "keychain"
  | "known-hosts"
  | "server-stats"
  | "fleet";

/**
 * A non-terminal tab in the top strip, drawn after the terminal tabs. Page tabs
 * stay mounted while hidden, so a screen keeps its unsaved state when the user
 * switches away. "new" is the launcher and can be open several times; every
 * other kind has at most one tab.
 */
export type PageTabKind = "new" | "settings";

export type PageTab = { id: string; kind: PageTabKind };

type UiState = {
  /** What the main area shows to the right of the sidebar. */
  mainView: MainView;
  navOpen: boolean;
  toggleNav: () => void;
  openNav: () => void;
  /** Rail-icon behavior: show the section's screen in the main area. */
  selectSection: (section: SidebarSection) => void;
  /** Force a section's screen into the main area (deep links / shortcuts). */
  openSection: (section: SidebarSection) => void;
  /** Show the terminal workspace in the main area (top-tab driven). */
  showTerminal: () => void;
  /** Open the Settings tab, or switch to it if it is already open. */
  openSettings: () => void;
  openKeychain: () => void;
  /** Show the known-hosts trust-store manager in the main area. */
  openKnownHosts: () => void;
  /** Show the agentless server stats dashboard in the main area. */
  openServerStats: () => void;
  /** Show foreground health checks across favorite hosts. */
  openFleet: () => void;
  terminalSearchOpen: boolean;
  setTerminalSearchOpen: (open: boolean) => void;
  /** Mobile terminal selection mode: one finger drags out a text selection
   * instead of driving the arrow-pad gesture. The two are exclusive. */
  terminalSelectMode: boolean;
  setTerminalSelectMode: (on: boolean) => void;
  pageTabs: PageTab[];
  /** The page tab on screen; null means a terminal tab (or the empty state). */
  activePageTabId: string | null;
  /** Open a fresh New tab launcher. */
  openNewTab: () => void;
  /** Open a page tab of a single-instance kind, or switch to its existing tab. */
  openPageTab: (kind: Exclude<PageTabKind, "new">) => void;
  selectPageTab: (tabId: string) => void;
  /** Close a page tab; with no id, the active one. */
  closePageTab: (tabId?: string) => void;
  /** A terminal opened from the active launcher replaces it. Leaves any other
   * kind of page tab open. */
  consumeLauncher: () => void;
  /** Serial-terminal connect dialog visibility. */
  serialConnectOpen: boolean;
  openSerialConnect: () => void;
  closeSerialConnect: () => void;
  /** Command palette overlay visibility. */
  paletteOpen: boolean;
  openPalette: () => void;
  closePalette: () => void;
  togglePalette: () => void;
  /** Collaboration (share/join) dialog visibility. */
  collabOpen: boolean;
  /** Why the collab dialog was opened: a pending capability join or an error to
   * surface. Never carries secrets beyond the opaque join token. */
  collabIntent: CollabIntent | null;
  openCollab: (intent?: CollabIntent) => void;
  closeCollab: () => void;
  clearCollabIntent: () => void;
  /** Join-a-vault dialog visibility, and the deep link that prefills it. The
   * link names a remote only — it carries no passphrase and no credentials. */
  vaultJoinOpen: boolean;
  vaultJoinLink: VaultJoinLink | null;
  openVaultJoin: (link?: VaultJoinLink) => void;
  closeVaultJoin: () => void;
  /** Web-preview dialog: the host whose listeners are being previewed, and an
   * optional label for the dialog subtitle. Null host means closed. */
  webPreviewHostId: string | null;
  webPreviewHostLabel: string | null;
  /** Terminal session the dialog was opened from, so previews it starts are
   * torn down with that session. */
  webPreviewSessionId: string | null;
  openWebPreview: (hostId: string, hostLabel?: string, sessionId?: string) => void;
  closeWebPreview: () => void;
  /** Multiplexer workspace dialog: the host whose tmux/zellij sessions are being
   * managed, plus an optional label for the subtitle. Null host means closed. */
  multiplexerHostId: string | null;
  multiplexerHostLabel: string | null;
  openMultiplexer: (hostId: string, hostLabel?: string) => void;
  closeMultiplexer: () => void;
  /** Docker dialog: the host whose containers are being managed, plus an
   * optional label for the subtitle. Null host means closed. */
  dockerHostId: string | null;
  dockerHostLabel: string | null;
  openDocker: (hostId: string, hostLabel?: string) => void;
  closeDocker: () => void;
  /** Repository (git status / diff) dialog: the host + working directory being
   * inspected, the session whose prompt an insertion targets, and an optional
   * label for the subtitle. Null target means closed. */
  repoTarget: {
    hostId: string;
    cwd: string;
    sessionId: string;
    label?: string;
  } | null;
  openRepo: (target: { hostId: string; cwd: string; sessionId: string; label?: string }) => void;
  closeRepo: () => void;
  /** Voice composer dialog: the session a composed draft is destined for, plus
   * an optional label for the subtitle. Null target means closed. */
  voiceTarget: {
    sessionId: string;
    label?: string;
  } | null;
  openVoice: (target: { sessionId: string; label?: string }) => void;
  closeVoice: () => void;
};

export type CollabIntent = {
  mode: "join";
  /** Opaque capability token to auto-redeem once the account is signed in. */
  joinToken?: string;
  /** User-facing error to show in the dialog (bad link, wrong server, join failure). */
  error?: string;
};

/** Closing a tab by id (its close button) moves to the neighbouring page tab.
 * Closing the active one implicitly (a terminal replacing its launcher, the
 * close-pane chord) reveals the terminal underneath instead. */
function removePageTab(
  state: Pick<UiState, "pageTabs" | "activePageTabId">,
  tabId?: string,
): Partial<UiState> {
  const closingId = tabId ?? state.activePageTabId;
  if (!closingId) return {};
  const index = state.pageTabs.findIndex((tab) => tab.id === closingId);
  if (index < 0) return {};
  const pageTabs = state.pageTabs.filter((tab) => tab.id !== closingId);
  const activePageTabId =
    state.activePageTabId === closingId
      ? tabId
        ? (pageTabs[Math.min(index, pageTabs.length - 1)]?.id ?? null)
        : null
      : state.activePageTabId;
  return { pageTabs, activePageTabId };
}

export const useUiStore = create<UiState>((set) => ({
  // With no terminal tabs open on launch, the main area defaults to Hosts.
  mainView: "hosts",
  navOpen: false,
  toggleNav: () => set((state) => ({ navOpen: !state.navOpen })),
  openNav: () => set({ navOpen: true }),
  selectSection: (section) => set({ mainView: section }),
  openSection: (section) => set({ mainView: section }),
  showTerminal: () => set({ mainView: "terminal", activePageTabId: null }),
  openSettings: () => useUiStore.getState().openPageTab("settings"),
  openKeychain: () => set({ mainView: "keychain" }),
  openKnownHosts: () => set({ mainView: "known-hosts" }),
  openServerStats: () => set({ mainView: "server-stats" }),
  openFleet: () => set({ mainView: "fleet" }),
  terminalSearchOpen: false,
  setTerminalSearchOpen: (open) => set({ terminalSearchOpen: open }),
  terminalSelectMode: false,
  setTerminalSelectMode: (on) => set({ terminalSelectMode: on }),
  pageTabs: [],
  activePageTabId: null,
  openNewTab: () =>
    set((state) => {
      const tab: PageTab = { id: crypto.randomUUID(), kind: "new" };
      return {
        mainView: "terminal",
        pageTabs: [...state.pageTabs, tab],
        activePageTabId: tab.id,
      };
    }),
  openPageTab: (kind) =>
    set((state) => {
      const existing = state.pageTabs.find((tab) => tab.kind === kind);
      if (existing) return { mainView: "terminal", activePageTabId: existing.id };
      const tab: PageTab = { id: crypto.randomUUID(), kind };
      return {
        mainView: "terminal",
        pageTabs: [...state.pageTabs, tab],
        activePageTabId: tab.id,
      };
    }),
  selectPageTab: (tabId) =>
    set((state) =>
      state.pageTabs.some((tab) => tab.id === tabId)
        ? { mainView: "terminal", activePageTabId: tabId }
        : {},
    ),
  closePageTab: (tabId) => set((state) => removePageTab(state, tabId)),
  consumeLauncher: () =>
    set((state) => {
      const active = state.pageTabs.find((tab) => tab.id === state.activePageTabId);
      return active?.kind === "new" ? removePageTab(state) : {};
    }),
  serialConnectOpen: false,
  openSerialConnect: () => set({ serialConnectOpen: true }),
  closeSerialConnect: () => set({ serialConnectOpen: false }),
  webPreviewHostId: null,
  webPreviewHostLabel: null,
  webPreviewSessionId: null,
  openWebPreview: (hostId, hostLabel, sessionId) =>
    set({
      webPreviewHostId: hostId,
      webPreviewHostLabel: hostLabel ?? null,
      webPreviewSessionId: sessionId ?? null,
    }),
  closeWebPreview: () =>
    set({
      webPreviewHostId: null,
      webPreviewHostLabel: null,
      webPreviewSessionId: null,
    }),
  multiplexerHostId: null,
  multiplexerHostLabel: null,
  openMultiplexer: (hostId, hostLabel) =>
    set({ multiplexerHostId: hostId, multiplexerHostLabel: hostLabel ?? null }),
  closeMultiplexer: () => set({ multiplexerHostId: null, multiplexerHostLabel: null }),
  dockerHostId: null,
  dockerHostLabel: null,
  openDocker: (hostId, hostLabel) =>
    set({ dockerHostId: hostId, dockerHostLabel: hostLabel ?? null }),
  closeDocker: () => set({ dockerHostId: null, dockerHostLabel: null }),
  repoTarget: null,
  openRepo: (target) => set({ repoTarget: target }),
  closeRepo: () => set({ repoTarget: null }),
  voiceTarget: null,
  openVoice: (target) => set({ voiceTarget: target }),
  closeVoice: () => set({ voiceTarget: null }),
  paletteOpen: false,
  openPalette: () => set({ paletteOpen: true }),
  closePalette: () => set({ paletteOpen: false }),
  togglePalette: () => set((state) => ({ paletteOpen: !state.paletteOpen })),
  collabOpen: false,
  collabIntent: null,
  // A no-arg open preserves any pending intent (e.g. a join token awaiting
  // sign-in) so reopening the dialog after signing in still auto-retries.
  openCollab: (intent) =>
    set((state) => ({
      collabOpen: true,
      collabIntent: intent ?? state.collabIntent,
    })),
  // Reset the transient intent on close, but keep a pending join token alive so
  // the sign-in → reopen → auto-join flow survives leaving for settings.
  closeCollab: () =>
    set((state) => ({
      collabOpen: false,
      collabIntent: state.collabIntent?.joinToken
        ? { mode: "join", joinToken: state.collabIntent.joinToken }
        : null,
    })),
  clearCollabIntent: () => set({ collabIntent: null }),
  vaultJoinOpen: false,
  vaultJoinLink: null,
  openVaultJoin: (link) => set({ vaultJoinOpen: true, vaultJoinLink: link ?? null }),
  closeVaultJoin: () => set({ vaultJoinOpen: false, vaultJoinLink: null }),
}));
