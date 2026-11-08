import { lazy, Suspense } from "react";
import { Settings, SquarePlus, type LucideIcon } from "lucide-react";
import type { PageTab, PageTabKind } from "../../stores/uiStore";
import { cn } from "../../lib/utils";
import { NewTabLauncher } from "./NewTabLauncher";

const SettingsScreen = lazy(() =>
  import("../settings/SettingsScreen").then((m) => ({ default: m.SettingsScreen })),
);

export const PAGE_TAB_META: Record<PageTabKind, { title: string; icon: LucideIcon }> = {
  new: { title: "New tab", icon: SquarePlus },
  settings: { title: "Settings", icon: Settings },
};

function PageContent({ kind }: { kind: Exclude<PageTabKind, "new"> }) {
  switch (kind) {
    case "settings":
      return <SettingsScreen />;
  }
}

/** Renders a page tab over the terminal workspace. Hidden tabs stay mounted so a
 * half-filled form survives switching to another tab. */
export function PageTabView({ tab, active }: { tab: PageTab; active: boolean }) {
  if (tab.kind === "new") return <NewTabLauncher tabId={tab.id} active={active} />;
  return (
    <div className={cn("absolute inset-0 z-20 bg-background", !active && "hidden")}>
      <Suspense
        fallback={
          <div className="flex h-full items-center justify-center text-sm text-muted">Loading…</div>
        }
      >
        <PageContent kind={tab.kind} />
      </Suspense>
    </div>
  );
}
