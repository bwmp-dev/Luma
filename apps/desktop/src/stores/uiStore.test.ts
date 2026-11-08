import { beforeEach, describe, expect, it } from "vitest";
import { useSessionStore } from "./sessionStore";
import { useUiStore } from "./uiStore";

beforeEach(() => {
  useSessionStore.setState({ sessions: [], tabs: [], activeSessionId: null, activeTabId: null });
  useUiStore.setState({
    mainView: "hosts",
    pageTabs: [],
    activePageTabId: null,
  });
});

describe("new tab launchers", () => {
  it("keeps multiple launchers and selects the newest one", () => {
    useUiStore.getState().openNewTab();
    const first = useUiStore.getState().activePageTabId;
    useUiStore.getState().openNewTab();
    const state = useUiStore.getState();

    expect(first).not.toBeNull();
    expect(state.pageTabs).toHaveLength(2);
    expect(state.activePageTabId).toBe(state.pageTabs[1].id);
  });

  it("selects an adjacent launcher when the active one is explicitly closed", () => {
    useUiStore.getState().openNewTab();
    useUiStore.getState().openNewTab();
    const [first, second] = useUiStore.getState().pageTabs;

    useUiStore.getState().closePageTab(second.id);

    expect(useUiStore.getState().pageTabs).toEqual([first]);
    expect(useUiStore.getState().activePageTabId).toBe(first.id);
  });

  it("preserves blank launchers when returning to a real terminal", () => {
    useUiStore.getState().openNewTab();
    const launcher = useUiStore.getState().pageTabs[0];

    useUiStore.getState().showTerminal();

    expect(useUiStore.getState().pageTabs).toEqual([launcher]);
    expect(useUiStore.getState().activePageTabId).toBeNull();
  });

  it("stays on the terminal empty state when the last launcher is closed", () => {
    useUiStore.getState().openNewTab();
    useSessionStore.getState().closeActivePane();
    expect(useUiStore.getState().pageTabs).toEqual([]);
    expect(useUiStore.getState().mainView).toBe("terminal");
  });
});

describe("settings tab", () => {
  it("opens once and switches back to the existing tab", () => {
    useUiStore.getState().openSettings();
    const settings = useUiStore.getState().activePageTabId;
    useUiStore.getState().openNewTab();
    useUiStore.getState().openSettings();

    const state = useUiStore.getState();
    expect(state.mainView).toBe("terminal");
    expect(state.pageTabs.filter((tab) => tab.kind === "settings")).toHaveLength(1);
    expect(state.activePageTabId).toBe(settings);
  });

  it("stays open when a terminal opens while it is showing", () => {
    useUiStore.getState().openSettings();
    const settings = useUiStore.getState().pageTabs[0];

    useUiStore.getState().consumeLauncher();

    expect(useUiStore.getState().pageTabs).toEqual([settings]);
    expect(useUiStore.getState().activePageTabId).toBe(settings.id);
  });

  it("stays open while another screen is shown", () => {
    useUiStore.getState().openSettings();
    const settings = useUiStore.getState().pageTabs[0];

    useUiStore.getState().selectSection("hosts");
    useUiStore.getState().selectPageTab(settings.id);

    expect(useUiStore.getState().mainView).toBe("terminal");
    expect(useUiStore.getState().activePageTabId).toBe(settings.id);
  });

  it("closes from the close-pane chord", () => {
    useUiStore.getState().openSettings();
    useSessionStore.getState().closeActivePane();
    expect(useUiStore.getState().pageTabs).toEqual([]);
  });
});

describe("consuming a launcher", () => {
  it("closes the active launcher and reveals the terminal", () => {
    useUiStore.getState().openSettings();
    const settings = useUiStore.getState().pageTabs[0];
    useUiStore.getState().openNewTab();

    useUiStore.getState().consumeLauncher();

    expect(useUiStore.getState().pageTabs).toEqual([settings]);
    expect(useUiStore.getState().activePageTabId).toBeNull();
  });
});
