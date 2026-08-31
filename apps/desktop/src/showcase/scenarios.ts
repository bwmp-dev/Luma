import { useUiStore } from "../stores/uiStore";
import { useSessionStore } from "../stores/sessionStore";
import { useMobileNavStore } from "../stores/mobileNavStore";
import { useServerStatsStore } from "../stores/serverStatsStore";
import { useAgentInboxStore } from "../stores/agentInboxStore";
import { terminalManager } from "../features/terminal/terminalManager";
import { STATS_HOST } from "./seed";

/* The mobile shell renders the same DOM on both phones; only the tab bar
 * differs, and on Android that is the web capsule this harness already draws.
 * So a scene is written once and captured for either platform. */
export type ShowcasePlatform = "desktop" | "ios" | "android";

export type ShowcaseView =
  | "terminal"
  // The Connections list, whose cards hold the open sessions' own terminals
  // (see MobileTerminalPreview). Mobile-only, and worth a scene of its own:
  // a preview can only be judged next to the session it shows.
  | "connections"
  | "hosts"
  | "snippets"
  | "settings"
  | "palette"
  // Mobile-only scenes for the App Store shots. Each is a route in the mobile
  // shell that has no desktop equivalent worth capturing on its own.
  | "vaults"
  | "servers"
  | "agent-inbox"
  | "sftp"
  | "delete-account";

export const SHOWCASE_VIEWS: ShowcaseView[] = [
  "terminal",
  "connections",
  "hosts",
  "snippets",
  "settings",
  "palette",
  "vaults",
  "servers",
  "agent-inbox",
  "sftp",
  "delete-account",
];

export function isShowcaseView(value: string): value is ShowcaseView {
  return (SHOWCASE_VIEWS as string[]).includes(value);
}

export function settleMs(view: ShowcaseView): number {
  // Both terminal scenes wait on the same thing: the mocked session output
  // arriving and xterm painting it. A card shows nothing its session has not
  // rendered yet, so the list needs that long too.
  return view === "terminal" || view === "connections" ? 1900 : 650;
}

const frame = () =>
  new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

/** Wait for the mobile shell to mount. Navigation itself is driven through
 * mobileNavStore rather than by clicking the tab bar, because on iOS the bar can
 * be a native view with no DOM to query. */
async function waitForMobileShell(): Promise<void> {
  const deadline = performance.now() + 10_000;
  while (performance.now() < deadline) {
    if (document.querySelector("#main-content")) return;
    await frame();
  }
  throw new Error("[showcase] mobile shell did not render");
}

async function setupTerminal(singleSession = false): Promise<void> {
  const store = useSessionStore.getState();
  await store.openSshSession("h-web-01", "vps-0cd97c22", "158.69.198.249", false, "#4ade80");
  if (singleSession) {
    await frame();
    return;
  }
  const primaryTabId = useSessionStore.getState().activeTabId;

  await store.splitActivePaneWith("row", {
    kind: "ssh",
    hostId: "h-db-01",
    title: "db-primary",
    connectionTarget: "10.0.4.20",
    tabColor: "#60a5fa",
  });

  await store.openSshSession("h-nas", "homelab-nas", "192.168.1.10");
  await store.openSshSession("h-edge", "edge-fedora", "203.0.113.9");

  if (primaryTabId) store.setActiveTab(primaryTabId);
  await frame();
}

/* The dashboard computes utilization from the delta between two snapshots, so
 * on first paint every CPU meter is empty and reads "waiting for next sample…".
 * Nothing but a second fetch fixes that, and the only handle on one from out
 * here is the button the user would press. */
async function clickButtonByText(
  label: string,
  /* Some controls are a whole row with the label nested inside — the SFTP host
   * picker makes the entire host row the button and "Connect" is one span of
   * it — so exact matching is not always possible. */
  match: "exact" | "contains" = "exact",
): Promise<void> {
  const deadline = performance.now() + 5_000;
  while (performance.now() < deadline) {
    const button = [...document.querySelectorAll("button")].find((candidate) => {
      const text = candidate.textContent?.trim() ?? "";
      return match === "exact" ? text === label : text.includes(label);
    });
    if (button) {
      button.click();
      return;
    }
    await frame();
  }
  throw new Error(`[showcase] no "${label}" button to click`);
}

/**
 * Wait for a screen to finish loading, identified by text only it renders.
 * Accepts several needles and reports which one won, for screens that can
 * settle into more than one state.
 */
async function waitForText(...needles: string[]): Promise<string> {
  const deadline = performance.now() + 5_000;
  while (performance.now() < deadline) {
    const body = document.body.textContent ?? "";
    const hit = needles.find((needle) => body.includes(needle));
    if (hit) return hit;
    await frame();
  }
  throw new Error(`[showcase] none of ${needles.join(" / ")} appeared`);
}

/* The inbox is fed by terminal output heuristics, not by a command, so there is
 * no invoke to mock — the events are pushed straight into the store.
 *
 * Two constraints shape this. Items are keyed by (terminal session, agent
 * session), so each row needs its OWN agent session id or later events collapse
 * onto earlier rows. And the screen greys out any item whose terminal session
 * is not currently live, so the ids have to be the BACKEND ids of sessions the
 * showcase actually opened — invented ones render as "session closed". */
const INBOX_EVENTS = [
  {
    agent: "claude-code",
    event: "needs-approval",
    title: "Approve edit to src/api/routes.ts",
    detail: "Rewrites the auth middleware to validate the session cookie",
    secondsAgo: 45,
  },
  {
    agent: "codex",
    event: "waiting-for-input",
    title: "Which database should the migration target?",
    detail: "staging or production",
    secondsAgo: 260,
  },
  {
    agent: "claude-code",
    event: "turn-completed",
    title: "Added integration tests for the transfer queue",
    detail: "14 files changed, all tests passing",
    secondsAgo: 900,
  },
  {
    agent: "gemini",
    event: "limit-warning",
    title: "Approaching the context limit",
    detail: "92% of the window used",
    secondsAgo: 2_400,
  },
];

async function seedAgentInbox(): Promise<void> {
  const store = useAgentInboxStore.getState();
  if (store.items.length > 0) return;

  // The rows describe agents running in terminals, so there have to be
  // terminals. setupTerminal is idempotent here because of the length check.
  if (useSessionStore.getState().tabs.length === 0) await setupTerminal();

  const backendIds = useSessionStore
    .getState()
    .sessions.map((session) => terminalManager.getBackendId(session.id))
    .filter((id): id is string => Boolean(id));
  if (backendIds.length === 0) return;

  // Relative ("2m ago") rather than absolute, so the labels read the same on
  // every capture run without pinning the clock.
  const now = Date.now();
  // Oldest first: the list is newest-updated first, which puts the item that
  // wants attention at the top.
  [...INBOX_EVENTS].reverse().forEach((item, index) => {
    store.recordEvent({
      terminalSessionId: backendIds[index % backendIds.length],
      agentSessionId: `${item.agent}-${index}`,
      agent: item.agent,
      event: item.event,
      title: item.title,
      detail: item.detail,
      ts: Math.round(now / 1000) - item.secondsAgo,
      source: "hook",
    });
  });
}

export async function applyScenario(
  view: ShowcaseView,
  platform: ShowcasePlatform = "desktop",
): Promise<void> {
  if (platform !== "desktop") {
    const nav = useMobileNavStore.getState();
    // A full-screen session replaces the whole shell — including the landmark
    // waitForMobileShell looks for — so step out of it before waiting, whatever
    // the target scene is. Only matters when switching scenes within one page
    // load, which is what the simulator capture does.
    nav.setFullscreen(false);
    await waitForMobileShell();
    if (view === "terminal") {
      /* Every scene may be applied more than once in a page load (boot renders
       * it, then the scenario channel replays it), so this has to describe the
       * end state rather than the transition into it: open sessions only if
       * there are none, then always put one full-screen. Branching on "already
       * has sessions" instead left the replay sitting on the Connections list.
       * navigate() clears the fullscreen flag, so it has to come first. */
      if (useSessionStore.getState().tabs.length === 0) {
        await setupTerminal(true);
      }
      nav.navigate("connections");
      nav.setFullscreen(true);
      await frame();
      return;
    }
    if (view === "connections") {
      /* Same end-state rule as the terminal scene: open sessions only if there
       * are none, then always land on the list rather than inside one. The
       * split in setupTerminal leaves the list with several cards, which is
       * what shows that each one holds its own session. */
      if (useSessionStore.getState().tabs.length === 0) {
        await setupTerminal();
      }
      nav.navigate("connections");
      nav.setFullscreen(false);
      await frame();
      return;
    }
    // Hosts and Snippets are now screens pushed from the Vaults tab; Settings
    // lives under Profile. The monitoring and file screens hang off Vaults too;
    // only the Agent Inbox lives under Connections.
    if (view === "hosts") nav.navigate("vaults", "hosts");
    else if (view === "snippets") nav.navigate("vaults", "snippets");
    else if (view === "settings") nav.navigate("profile");
    else if (view === "vaults") nav.navigate("vaults");
    else if (view === "servers") {
      // The dashboard shows a host picker until one is selected.
      useServerStatsStore.getState().select(STATS_HOST);
      nav.navigate("vaults", "servers");
      await frame();
      await clickButtonByText("Refresh");
    } else if (view === "sftp") {
      nav.navigate("vaults", "sftp");
      /* The screen opens on a host picker and only shows the browser once a
       * host is connected — but the connection outlives the scene, so a second
       * capture pass (the light theme) lands straight on the browser. Wait for
       * whichever appears, and connect only if it is the picker. Waiting also
       * steps over the push transition, during which the outgoing screen is
       * still mounted and would match first. */
      const settled = await waitForText(
        "docker-compose.yml",
        "Connect to a saved host",
      );
      if (settled !== "docker-compose.yml") {
        // Match the row by host name rather than by "Connect" — every row
        // carries that word, and the listing is this host's home directory.
        await clickButtonByText(STATS_HOST.name, "contains");
        await waitForText("docker-compose.yml");
      }
    }
    else if (view === "delete-account") {
      // The account screen with the delete confirmation open: the surface App
      // Review looks for, and the one that can only be checked on a device.
      nav.navigate("profile", "settings-account");
      await frame();
      /* Exact match on the opener's label: the dialog's confirm button reads
       * "Delete account" without the ellipsis, and a loose match would click
       * that instead on the replay pass and actually run the deletion. Opening
       * an already-open dialog is a no-op, so this stays the end state. */
      await clickButtonByText("Delete account…");
    }
    else if (view === "agent-inbox") {
      await seedAgentInbox();
      nav.navigate("connections", "agent-inbox");
    }
    await frame();
    return;
  }
  const ui = useUiStore.getState();
  switch (view) {
    case "terminal":
      await setupTerminal();
      break;
    case "hosts":
      ui.openSection("hosts");
      break;
    case "snippets":
      ui.openSection("snippets");
      break;
    case "settings":
      ui.openSettings();
      break;
    case "palette":
      ui.openSection("hosts");
      ui.openPalette();
      break;
  }
}
