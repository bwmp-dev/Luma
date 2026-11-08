import { useSessionStore } from "../../stores/sessionStore";
import { useTabDragStore } from "../../stores/tabDragStore";
import { useUiStore } from "../../stores/uiStore";
import { HostsScreen } from "../hosts/HostsScreen";
import { PaneTreeView } from "./PaneTreeView";
import { SearchBar } from "./SearchBar";
import { PageTabView } from "./pageTabs";
import { useLayoutEffect, useSyncExternalStore } from "react";
import { detachedTabIds, detachedTabsVersion, subscribeDetachedTabs } from "./detachedTabs";

export function Workspace() {
  useSyncExternalStore(subscribeDetachedTabs, detachedTabsVersion, detachedTabsVersion);
  const sessions = useSessionStore((s) => s.sessions);
  const tabs = useSessionStore((s) => s.tabs);
  const activeTabId = useSessionStore((s) => s.activeTabId);
  const activeSessionId = useSessionStore((s) => s.activeSessionId);
  const setActiveTab = useSessionStore((s) => s.setActiveTab);
  const searchOpen = useUiStore((s) => s.terminalSearchOpen);
  const pageTabs = useUiStore((s) => s.pageTabs);
  const activePageTabId = useUiStore((s) => s.activePageTabId);
  const terminalShown = useUiStore((s) => s.mainView === "terminal");
  const tabDragActive = useTabDragStore((s) => s.sourceTabId !== null);

  const activeTab = tabs.find((t) => t.id === activeTabId && !detachedTabIds().has(t.id));
  const showingNothing = terminalShown && !activeTab && !activePageTabId && !tabDragActive;

  // The active tab can be one torn off into its own window (tearing off or
  // closing tabs, cycling). If a tab is still docked here, show it instead of
  // the host list; with none left, saved hosts remain available here.
  // Waits for a drag to end so a live tear-off can still be docked back.
  useLayoutEffect(() => {
    if (!showingNothing) return;
    const detached = detachedTabIds();
    const docked = tabs.find((t) => !detached.has(t.id));
    if (docked) setActiveTab(docked.id);
  }, [showingNothing, tabs, setActiveTab]);

  return (
    <div className="relative h-full min-w-0">
      {/* data-tab-drop-workspace carries the visible tab's id so a tab dragged
          from the strip into this area groups (splits) with it — see TabBar. */}
      <div
        className={activePageTabId ? "hidden" : "h-full"}
        data-tab-drop-workspace={activeTab?.id}
      >
        {activeTab ? (
          <>
            {searchOpen && activeSessionId && <SearchBar sessionId={activeSessionId} />}
            {/* Keying by tab id detaches the previous tab's terminals and attaches
              this tab's on switch, exactly matching the single-terminal flow. */}
            <PaneTreeView key={activeTab.id} tab={activeTab} sessions={sessions} />
          </>
        ) : showingNothing ? (
          <HostsScreen />
        ) : null}
      </div>
      {pageTabs.map((tab) => (
        <PageTabView key={tab.id} tab={tab} active={tab.id === activePageTabId} />
      ))}
    </div>
  );
}
