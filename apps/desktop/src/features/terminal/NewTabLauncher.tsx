import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import {
  Cable,
  FolderKanban,
  Layers,
  Loader2,
  PlugZap,
  Search,
  Server,
  SquareTerminal,
  Star,
  X,
} from "lucide-react";
import { useHostGroups, useHosts, useRecentHosts } from "../../hooks/useHosts";
import { useProfiles, useShells } from "../../hooks/useShells";
import { useBrowsingVaultId } from "../../stores/vaultStore";
import { useSessionStore } from "../../stores/sessionStore";
import { useUiStore } from "../../stores/uiStore";
import {
  buildHostGroupLayout,
  countTemplatePanes,
  useTemplateStore,
} from "../../stores/templateStore";
import { cn } from "../../lib/utils";
import { looksLikeConnectionString } from "../../lib/connectionString";
import { parseLumaError, quickConnectPrepare } from "../../lib/hosts";
import type { ShellRef } from "../../lib/terminal";
import { DistroIcon } from "../../components/DistroIcon";
import { ShellIcon } from "./ShellIcon";
import { terminalManager } from "./terminalManager";
import { detachedTabIds } from "./detachedTabs";

type LocalLauncher = {
  key: string;
  ref: ShellRef;
  name: string;
  hint: string | null;
  icon: ReactNode;
};

type LauncherItem = { key: string; run: () => void };

const sectionHeading = "px-3 pb-1.5 text-[11px] font-semibold uppercase tracking-wider text-muted";

export function NewTabLauncher({ tabId, active }: { tabId: string; active: boolean }) {
  const [query, setQuery] = useState("");
  const [picked, setPicked] = useState<number | null>(null);
  const [quickError, setQuickError] = useState<string | null>(null);
  const [quickBusy, setQuickBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const idPrefix = useId();
  const browsingVaultId = useBrowsingVaultId();
  const { data: hosts = [] } = useHosts(browsingVaultId);
  const { data: recent = [] } = useRecentHosts();
  const { data: groups = [] } = useHostGroups(browsingVaultId);
  const { data: shells = [] } = useShells();
  const { data: profiles = [] } = useProfiles();
  const templates = useTemplateStore((s) => s.templates);
  const removeTemplate = useTemplateStore((s) => s.removeTemplate);
  const openSshSession = useSessionStore((s) => s.openSshSession);
  const openLocalSession = useSessionStore((s) => s.openLocalSession);
  const openTemplate = useSessionStore((s) => s.openTemplate);
  const hasOpenTab = useSessionStore((s) => s.tabs.length > 0);
  const closeNewTab = useUiStore((s) => s.closeNewTab);
  const openSerialConnect = useUiStore((s) => s.openSerialConnect);

  useEffect(() => {
    if (!active) return;
    const previous = document.activeElement;
    if (
      previous instanceof HTMLElement &&
      previous !== document.body &&
      !rootRef.current?.contains(previous)
    ) {
      previousFocusRef.current = previous;
    }
    inputRef.current?.focus();
  }, [active]);

  // Dismissing must not strand focus on <body>. The next launcher (if one becomes
  // active) focuses its own input; otherwise the revealed terminal takes focus,
  // and failing that, whatever held focus before this launcher opened. Deferred a
  // frame so the workspace is unhidden before xterm is asked to focus.
  const dismiss = () => {
    closeNewTab(tabId);
    requestAnimationFrame(() => {
      const ui = useUiStore.getState();
      if (ui.activeNewTabId) return;
      const { activeTabId, activeSessionId } = useSessionStore.getState();
      if (
        ui.mainView === "terminal" &&
        activeSessionId &&
        activeTabId &&
        !detachedTabIds().has(activeTabId)
      ) {
        terminalManager.focus(activeSessionId);
        return;
      }
      const previous = previousFocusRef.current;
      if (previous?.isConnected) previous.focus();
    });
  };
  const dismissRef = useRef(dismiss);
  dismissRef.current = dismiss;

  // Escape dismisses the launcher regardless of what currently holds focus (the
  // search input, a template's delete button, or nothing). Capture phase so it
  // fires before any focused surface handles the key and can't be swallowed.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!active || event.key !== "Escape") return;
      // A dialog on top (the command palette, a confirm) owns its own Escape.
      if (document.querySelector('[role="dialog"]')) return;
      event.preventDefault();
      event.stopPropagation();
      dismissRef.current();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [active]);

  const needle = query.trim().toLowerCase();
  const matchingHosts = useMemo(() => {
    // Recents are per-device history across every vault, so they need the same
    // scope applied that useHosts already applied to `hosts`.
    const scopedRecent = browsingVaultId
      ? recent.filter((host) => host.vaultId === browsingVaultId)
      : recent;
    const source = needle ? hosts : scopedRecent.length ? scopedRecent : hosts;
    return source.filter((host) =>
      [host.name, host.hostname, host.username, ...host.tags]
        .filter(Boolean)
        .some((value) => String(value).toLowerCase().includes(needle)),
    );
  }, [hosts, recent, needle, browsingVaultId]);
  const matchingGroups = groups
    .filter((group) => group.name.toLowerCase().includes(needle))
    .map((group) => ({
      group,
      count: hosts.filter((host) => host.groupId === group.id).length,
    }));
  const matchingTemplates = templates.filter((template) =>
    template.name.toLowerCase().includes(needle),
  );
  const localLaunchers = useMemo<LocalLauncher[]>(
    () => [
      ...shells.map(
        (shell): LocalLauncher => ({
          key: `shell:${shell.id}`,
          ref: { kind: "shell", id: shell.id },
          name: shell.name,
          hint: shell.kind === "wsl" ? "WSL" : null,
          icon: <ShellIcon shell={shell} size={15} />,
        }),
      ),
      ...profiles.map(
        (profile): LocalLauncher => ({
          key: `profile:${profile.id}`,
          ref: { kind: "profile", id: profile.id },
          name: profile.name,
          hint: "Profile",
          icon: <SquareTerminal size={15} />,
        }),
      ),
    ],
    [shells, profiles],
  );
  const matchingLaunchers = localLaunchers.filter((launcher) =>
    [launcher.name, launcher.hint].some((value) => value?.toLowerCase().includes(needle)),
  );

  const connect = (host: (typeof hosts)[number]) =>
    void openSshSession(host.id, host.name, host.hostname, false, host.tabColor);

  const trimmedQuery = query.trim();
  const isConnectionString = looksLikeConnectionString(trimmedQuery);
  // Quick connect: parse the typed connection string into an ephemeral host on
  // the backend, then launch it through the normal SSH connect flow.
  const runQuickConnect = () => {
    if (!trimmedQuery || quickBusy) return;
    setQuickError(null);
    setQuickBusy(true);
    quickConnectPrepare(trimmedQuery)
      .then((host) => {
        closeNewTab();
        void openSshSession(host.id, host.name, host.hostname, true);
      })
      .catch((error) => setQuickError(parseLumaError(error).message))
      .finally(() => setQuickBusy(false));
  };
  // A host group opens as ONE grouped (split) tab reproducing an even layout of
  // all its hosts, rather than a separate tab per host.
  const openGroup = (groupId: string) => {
    const groupHosts = hosts.filter((host) => host.groupId === groupId);
    const root = buildHostGroupLayout(groupHosts);
    if (root) {
      closeNewTab();
      openTemplate(root);
    }
  };

  const items: LauncherItem[] = [];
  if (isConnectionString) items.push({ key: "quick-connect", run: runQuickConnect });
  if (!needle) {
    items.push(
      { key: "local", run: () => void openLocalSession() },
      {
        key: "serial",
        run: () => {
          closeNewTab();
          openSerialConnect();
        },
      },
    );
  }
  const firstHostIndex = items.length;
  for (const host of matchingHosts)
    items.push({ key: `host:${host.id}`, run: () => connect(host) });
  for (const launcher of matchingLaunchers) {
    items.push({
      key: launcher.key,
      run: () => void openLocalSession(launcher.ref, launcher.name),
    });
  }
  for (const template of matchingTemplates) {
    items.push({
      key: `template:${template.id}`,
      run: () => {
        closeNewTab();
        openTemplate(template.root);
      },
    });
  }
  for (const { group, count } of matchingGroups) {
    if (count) items.push({ key: `group:${group.id}`, run: () => openGroup(group.id) });
  }
  const indexByKey = new Map(items.map((item, index) => [item.key, index]));

  // With no explicit pick, Enter keeps its long-standing meaning: the first
  // matching host, else quick connect for a connection string, else the first
  // launcher — each of which is the first item when no host matches.
  const defaultIndex = matchingHosts.length > 0 ? firstHostIndex : 0;
  const activeIndex = items.length ? Math.min(picked ?? defaultIndex, items.length - 1) : -1;
  const optionId = (index: number) => `${idPrefix}-option-${index}`;
  const listboxId = `${idPrefix}-listbox`;

  useEffect(() => {
    if (picked === null || activeIndex < 0) return;
    document
      .getElementById(`${idPrefix}-option-${activeIndex}`)
      ?.scrollIntoView({ block: "nearest" });
  }, [picked, activeIndex, idPrefix]);

  const onInputKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.nativeEvent.isComposing || !items.length) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setPicked(Math.min(activeIndex + 1, items.length - 1));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setPicked(Math.max(activeIndex - 1, 0));
    } else if (event.key === "Home") {
      event.preventDefault();
      setPicked(0);
    } else if (event.key === "End") {
      event.preventDefault();
      setPicked(items.length - 1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      items[activeIndex]?.run();
    }
  };

  const option = (key: string) => {
    const index = indexByKey.get(key) ?? -1;
    return {
      id: optionId(index),
      selected: index === activeIndex,
      onHover: () => setPicked(index),
      onSelect: items[index]?.run,
    };
  };

  return (
    <div
      ref={rootRef}
      data-new-tab-launcher={tabId}
      className={cn("@container absolute inset-0 z-20 bg-background", !active && "hidden")}
    >
      <button
        type="button"
        onClick={dismiss}
        aria-label={hasOpenTab ? "Close new tab and return to terminal" : "Close new tab"}
        title="Close (Esc)"
        className="absolute right-3 top-3 z-10 flex h-8 w-8 items-center justify-center rounded-md text-muted hover:bg-raised hover:text-foreground"
      >
        <X size={16} />
      </button>
      <div className="h-full overflow-y-auto px-6 pb-10 pt-14 @4xl:pt-8">
        <div className="mx-auto w-full max-w-3xl">
          <div className="relative">
            <Search
              size={16}
              className="pointer-events-none absolute left-4 top-1/2 -translate-y-1/2 text-muted"
            />
            <input
              ref={inputRef}
              value={query}
              onChange={(event) => {
                setQuery(event.target.value);
                setPicked(null);
              }}
              onKeyDown={onInputKeyDown}
              placeholder="Search hosts, or type user@host to connect"
              aria-label="Search hosts, workspace templates, or a connection string"
              role="combobox"
              aria-expanded
              aria-controls={listboxId}
              aria-autocomplete="list"
              aria-activedescendant={activeIndex >= 0 ? optionId(activeIndex) : undefined}
              className="h-11 w-full rounded-xl border border-border bg-surface pl-11 pr-16 text-sm outline-none transition focus:border-accent focus:ring-1 focus:ring-accent motion-reduce:transition-none"
            />
            <kbd className="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2 rounded border border-border bg-raised px-2 py-0.5 text-[10px] text-muted">
              ESC
            </kbd>
          </div>

          {quickError && (
            <p
              role="alert"
              className="mt-2 rounded-lg border border-danger/40 bg-danger/10 px-3 py-2 text-xs text-danger"
            >
              {quickError}
            </p>
          )}

          <div id={listboxId} role="listbox" aria-label="New tab options">
            {isConnectionString && (
              <div className="mt-3">
                <LauncherOption
                  {...option("quick-connect")}
                  disabled={quickBusy}
                  emphasis
                  icon={
                    quickBusy ? (
                      <Loader2 size={16} className="animate-spin motion-reduce:animate-none" />
                    ) : (
                      <PlugZap size={16} />
                    )
                  }
                  title={`Connect to ${trimmedQuery}`}
                  subtitle="Quick connect over SSH (not saved)"
                />
              </div>
            )}

            {!needle && (
              <div className="mt-4 grid gap-1 sm:grid-cols-2">
                <LauncherOption
                  {...option("local")}
                  accentIcon
                  icon={<SquareTerminal size={16} />}
                  title="Local terminal"
                  subtitle="Open your default shell"
                />
                <LauncherOption
                  {...option("serial")}
                  accentIcon
                  icon={<Cable size={16} />}
                  title="Serial terminal"
                  subtitle="Connect to a serial port"
                />
              </div>
            )}

            <div role="group" aria-labelledby={`${idPrefix}-hosts`} className="mt-6">
              <h2 id={`${idPrefix}-hosts`} className={sectionHeading}>
                {needle ? "Search results" : "Recent connections"}
              </h2>
              {matchingHosts.map((host) => (
                <LauncherOption
                  key={host.id}
                  {...option(`host:${host.id}`)}
                  icon={
                    host.osId && host.osId !== "unknown" ? (
                      <DistroIcon
                        osId={host.osId}
                        size={16}
                        label={host.osPrettyName ?? undefined}
                      />
                    ) : (
                      <Server size={15} />
                    )
                  }
                  title={
                    <>
                      <span className="truncate">{host.name}</span>
                      {host.favorite && (
                        <>
                          <Star
                            size={12}
                            fill="currentColor"
                            aria-hidden
                            className="shrink-0 text-accent"
                          />
                          <span className="sr-only">(favorite)</span>
                        </>
                      )}
                    </>
                  }
                  subtitle={`${host.username ? `${host.username}@` : ""}${host.hostname}:${host.port}`}
                  meta="SSH"
                />
              ))}
              {matchingHosts.length === 0 && (
                <p className="px-3 py-4 text-sm text-muted">
                  {needle ? `No hosts match “${query}”.` : "No saved hosts yet."}
                </p>
              )}
            </div>

            {matchingLaunchers.length > 0 && (
              <div role="group" aria-labelledby={`${idPrefix}-shells`} className="mt-6">
                <h2 id={`${idPrefix}-shells`} className={sectionHeading}>
                  Local shells
                </h2>
                <div className="grid gap-x-1 sm:grid-cols-2">
                  {matchingLaunchers.map((launcher) => (
                    <LauncherOption
                      key={launcher.key}
                      {...option(launcher.key)}
                      icon={launcher.icon}
                      title={<span className="truncate">{launcher.name}</span>}
                      meta={launcher.hint}
                    />
                  ))}
                </div>
              </div>
            )}

            {(matchingTemplates.length > 0 || matchingGroups.length > 0) && (
              <div role="group" aria-labelledby={`${idPrefix}-templates`} className="mt-6">
                <h2 id={`${idPrefix}-templates`} className={sectionHeading}>
                  Workspace templates
                </h2>
                {matchingTemplates.map((template) => {
                  const panes = countTemplatePanes(template.root);
                  return (
                    <LauncherOption
                      key={template.id}
                      {...option(`template:${template.id}`)}
                      accentIcon
                      icon={<Layers size={15} />}
                      title={<span className="truncate">{template.name}</span>}
                      meta={`${panes} pane${panes === 1 ? "" : "s"}`}
                      action={
                        <button
                          type="button"
                          aria-label={`Delete template ${template.name}`}
                          title="Delete template"
                          onClick={() => void removeTemplate(template.id)}
                          className="mr-2 shrink-0 rounded p-1 text-muted opacity-0 hover:bg-surface hover:text-danger focus-visible:opacity-100 group-hover:opacity-100"
                        >
                          <X size={14} />
                        </button>
                      }
                    />
                  );
                })}
                {matchingGroups.map(({ group, count }) =>
                  count ? (
                    <LauncherOption
                      key={group.id}
                      {...option(`group:${group.id}`)}
                      icon={<FolderKanban size={15} />}
                      title={<span className="truncate">{group.name}</span>}
                      subtitle="Opens as one grouped tab"
                      meta={`${count} connection${count === 1 ? "" : "s"}`}
                    />
                  ) : (
                    <LauncherOption
                      key={group.id}
                      disabled
                      icon={<FolderKanban size={15} />}
                      title={<span className="truncate">{group.name}</span>}
                      subtitle="No connections in this group"
                      meta="0 connections"
                    />
                  ),
                )}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

function LauncherOption({
  id,
  selected = false,
  disabled = false,
  emphasis = false,
  accentIcon = false,
  onHover,
  onSelect,
  icon,
  title,
  subtitle,
  meta,
  action,
}: {
  id?: string;
  selected?: boolean;
  disabled?: boolean;
  emphasis?: boolean;
  accentIcon?: boolean;
  onHover?: () => void;
  onSelect?: () => void;
  icon: ReactNode;
  title: ReactNode;
  subtitle?: ReactNode;
  meta?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div
      onMouseMove={disabled ? undefined : onHover}
      className={cn(
        "group flex min-w-0 items-center rounded-lg",
        emphasis && "border border-accent/50",
        selected ? "bg-accent/10" : emphasis && "bg-accent/5",
        disabled && "opacity-45",
      )}
    >
      <button
        type="button"
        id={id}
        role="option"
        tabIndex={-1}
        aria-selected={selected}
        aria-disabled={disabled || undefined}
        disabled={disabled}
        onClick={onSelect}
        className="flex min-h-11 min-w-0 flex-1 items-center gap-3 px-3 py-1 text-left text-sm"
      >
        <span
          className={cn(
            "flex h-8 w-8 shrink-0 items-center justify-center rounded-lg",
            accentIcon || emphasis ? "bg-accent/15 text-accent" : "bg-raised text-muted",
            selected && !accentIcon && !emphasis && "text-accent",
          )}
        >
          {icon}
        </span>
        <span className="min-w-0">
          <span className="flex min-w-0 items-center gap-1.5 font-medium">{title}</span>
          {subtitle && <span className="block truncate text-xs text-muted">{subtitle}</span>}
        </span>
        {(selected || meta) && (
          <span className="ml-auto flex shrink-0 items-center gap-2 text-xs text-muted">
            {selected && (
              <kbd className="rounded border border-border px-1.5 py-0.5 text-[10px]">Enter</kbd>
            )}
            {meta}
          </span>
        )}
      </button>
      {action}
    </div>
  );
}
