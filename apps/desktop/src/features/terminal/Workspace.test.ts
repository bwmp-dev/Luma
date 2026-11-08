import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Workspace } from "./Workspace";
import { useUiStore } from "../../stores/uiStore";
import { useSessionStore } from "../../stores/sessionStore";

vi.mock("../hosts/HostsPanel", () => ({ HostsPanel: () => "Saved hosts" }));
vi.mock("./PaneTreeView", () => ({ PaneTreeView: () => "Connected terminal" }));
vi.mock("./pageTabs", () => ({ PageTabView: () => null }));
vi.mock("./SearchBar", () => ({ SearchBar: () => null }));

let root: Root;
let container: HTMLDivElement;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  root = createRoot(container);
  useSessionStore.setState({ sessions: [], tabs: [], activeSessionId: null, activeTabId: null });
  useUiStore.setState({ mainView: "terminal", pageTabs: [], activePageTabId: null });
});

afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});

describe("closing the last page tab", () => {
  it.each(["new", "settings"] as const)("shows saved hosts after closing %s", async (kind) => {
    if (kind === "new") useUiStore.getState().openNewTab();
    else useUiStore.getState().openSettings();
    useSessionStore.getState().closeActivePane();
    expect(useUiStore.getState().activePageTabId).toBeNull();
    await act(async () => root.render(createElement(Workspace)));
    expect(container.textContent).toContain("Saved hosts");
  });

  it("does not mount a second hosts screen while another section is visible", async () => {
    useUiStore.getState().selectSection("hosts");
    await act(async () => root.render(createElement(Workspace)));
    expect(container.textContent).not.toContain("Saved hosts");
  });
});
