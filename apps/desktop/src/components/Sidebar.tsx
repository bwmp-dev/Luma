import { useRef } from "react";
import { KeyRound, ScrollText, Server, Settings, ShieldCheck, SquareCode } from "lucide-react";
import { useUiStore } from "../stores/uiStore";
import type { SidebarSection } from "../types";
import { cn } from "../lib/utils";

const ITEMS: { section: SidebarSection; label: string; icon: typeof Server }[] = [
  { section: "logs", label: "Logs", icon: ScrollText },
  { section: "snippets", label: "Snippets", icon: SquareCode },
];

type RailItem = {
  key: string;
  label: string;
  icon: typeof Server;
  active: boolean;
  onClick: () => void;
};

export function Sidebar() {
  const mainView = useUiStore((s) => s.mainView);
  const selectSection = useUiStore((s) => s.selectSection);
  const openSettings = useUiStore((s) => s.openSettings);
  const settingsTabActive = useUiStore(
    (s) =>
      s.mainView === "terminal" &&
      s.pageTabs.some((tab) => tab.id === s.activePageTabId && tab.kind === "settings"),
  );
  const openKeychain = useUiStore((s) => s.openKeychain);
  const openKnownHosts = useUiStore((s) => s.openKnownHosts);

  const items: RailItem[] = [
    {
      key: "hosts",
      label: "Hosts",
      icon: Server,
      active: mainView === "hosts",
      onClick: () => selectSection("hosts"),
    },
    {
      key: "keychain",
      label: "Keychain",
      icon: KeyRound,
      active: mainView === "keychain",
      onClick: openKeychain,
    },
    {
      key: "known-hosts",
      label: "Known hosts",
      icon: ShieldCheck,
      active: mainView === "known-hosts",
      onClick: openKnownHosts,
    },
    ...ITEMS.map((item) => ({
      key: item.section,
      label: item.label,
      icon: item.icon,
      active: mainView === item.section,
      onClick: () => selectSection(item.section),
    })),
    {
      key: "settings",
      label: "Settings",
      icon: Settings,
      active: settingsTabActive,
      onClick: openSettings,
    },
  ];
  // Roving tabindex: only the active (or first) rail button is in the Tab order;
  // Arrow/Home/End move focus between them.
  const activeIndex = Math.max(
    0,
    items.findIndex((item) => item.active),
  );
  const btnRefs = useRef<(HTMLButtonElement | null)[]>([]);

  const onKeyDown = (event: React.KeyboardEvent, index: number) => {
    let next = index;
    if (event.key === "ArrowDown") next = (index + 1) % items.length;
    else if (event.key === "ArrowUp") next = (index - 1 + items.length) % items.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = items.length - 1;
    else return;
    event.preventDefault();
    btnRefs.current[next]?.focus();
  };

  const lastIndex = items.length - 1;

  return (
    <div className="flex h-full shrink-0">
      <nav
        className="flex w-19 flex-col items-center border-r border-border bg-surface py-3"
        aria-label="Primary"
      >
        <div className="flex w-full flex-col gap-1 px-1.5">
          {items.slice(0, lastIndex).map((item, index) => (
            <RailButton
              key={item.key}
              item={item}
              tabIndex={index === activeIndex ? 0 : -1}
              buttonRef={(el) => {
                btnRefs.current[index] = el;
              }}
              onKeyDown={(event) => onKeyDown(event, index)}
            />
          ))}
        </div>
        <div className="flex-1" />
        <div className="w-full px-1.5">
          <RailButton
            item={items[lastIndex]}
            tabIndex={lastIndex === activeIndex ? 0 : -1}
            buttonRef={(el) => {
              btnRefs.current[lastIndex] = el;
            }}
            onKeyDown={(event) => onKeyDown(event, lastIndex)}
          />
        </div>
      </nav>
    </div>
  );
}

function RailButton({
  item,
  tabIndex,
  buttonRef,
  onKeyDown,
}: {
  item: RailItem;
  tabIndex: number;
  buttonRef: (el: HTMLButtonElement | null) => void;
  onKeyDown: (event: React.KeyboardEvent) => void;
}) {
  const { label, active, onClick, icon: Icon } = item;
  return (
    <button
      ref={buttonRef}
      type="button"
      title={label}
      aria-label={label}
      aria-pressed={active}
      aria-current={active ? "page" : undefined}
      tabIndex={tabIndex}
      onClick={onClick}
      onKeyDown={onKeyDown}
      className={cn(
        "relative flex h-13 w-full flex-col items-center justify-center gap-1 rounded-lg text-[11px] leading-none transition-colors",
        active
          ? "bg-raised font-medium text-foreground"
          : "text-muted hover:bg-raised hover:text-foreground",
      )}
    >
      {active && (
        <span
          aria-hidden="true"
          className="absolute inset-y-2.5 -left-1.5 w-0.75 rounded-r-full bg-accent"
        />
      )}
      <Icon size={18} strokeWidth={1.75} className={active ? "text-accent" : undefined} />
      <span className="whitespace-nowrap">{label}</span>
    </button>
  );
}
