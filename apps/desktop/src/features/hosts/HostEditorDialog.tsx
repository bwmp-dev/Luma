import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import {
  Cable,
  Check,
  ChevronDown,
  Contact,
  Folder,
  FolderOpen,
  KeyRound,
  LockKeyhole,
  Server,
  SquareTerminal,
  Tag,
  UserRound,
  Waypoints,
  type LucideIcon,
} from "lucide-react";
import { InspectorShell } from "../../components/InspectorShell";
import { DistroIcon } from "../../components/DistroIcon";
import { cn } from "../../lib/utils";
import {
  createHost,
  groupInheritedDefaults,
  inheritedGroupId,
  parseLumaError,
  updateHost,
  type AuthenticationType,
  type Host,
  type HostFieldOrigins,
  type HostGroup,
  type HostInput,
  type KeyReference,
  type Identity,
  type TransportType,
} from "../../lib/hosts";
import { useInvalidateHosts } from "../../hooks/useHosts";
import { CheckboxField, EnvironmentEditor, TabColorField, type EnvRow } from "./fields";

/** Fields a group can supply when the host leaves them unset. Authentication
 * and key stay host-only: their stored default cannot be told apart from a
 * deliberate choice, and inheriting them would change how a host authenticates. */
type InheritableField = Exclude<keyof HostFieldOrigins, "environment">;

type FormState = {
  name: string;
  hostname: string;
  port: string;
  username: string;
  groupId: string;
  authenticationType: AuthenticationType;
  keyId: string;
  identityId: string;
  proxyJumpHostId: string;
  startupCommand: string;
  workingDirectory: string;
  tags: string;
  favorite: boolean;
  /** Per-host tab accent color as "#RRGGBB", or "" for no accent. */
  tabColor: string;
  transport: TransportType;
  moshServerPath: string;
  moshPortRange: string;
  env: EnvRow[];
};

function initialState(host: Host | null, initialGroupId: string | null = null): FormState {
  return {
    name: host?.name ?? "",
    hostname: host?.hostname ?? "",
    port: String(host?.port ?? 22),
    username: host?.username ?? "",
    groupId: host?.groupId ?? initialGroupId ?? "",
    authenticationType: host?.authenticationType ?? "interactive",
    keyId: host?.keyId ?? "",
    identityId: host?.identityId ?? "",
    proxyJumpHostId: host?.proxyJumpHostId ?? "",
    startupCommand: host?.startupCommand ?? "",
    workingDirectory: host?.workingDirectory ?? "",
    tags: (host?.tags ?? []).join(", "),
    favorite: host?.favorite ?? false,
    tabColor: host?.tabColor ?? "",
    transport: host?.transport ?? "ssh",
    moshServerPath: host?.moshServerPath ?? "",
    moshPortRange: host?.moshPortRange ?? "",
    env: host?.environment
      ? Object.entries(host.environment).map(([key, value]) => ({ key, value }))
      : [],
  };
}

const AUTH_OPTIONS: { value: AuthenticationType; label: string }[] = [
  { value: "key", label: "Private key" },
  { value: "password", label: "Password (interactive)" },
  { value: "interactive", label: "Keyboard-interactive" },
];

const TRANSPORT_OPTIONS: { value: TransportType; label: string }[] = [
  { value: "ssh", label: "SSH" },
  { value: "auto", label: "Auto (Mosh with SSH fallback)" },
  { value: "mosh", label: "Mosh only" },
];

type FieldErrors = Partial<
  Record<
    | "name"
    | "hostname"
    | "username"
    | "port"
    | "keyId"
    | "groupId"
    | "identityId"
    | "proxyJumpHostId"
    | "moshServerPath"
    | "moshPortRange",
    string
  >
>;

/** Options the caller offered, already narrowed to the host's own vault. */
type References = {
  groups: HostGroup[];
  keyReferences: KeyReference[];
  identities: Identity[];
  hosts: Host[];
};

/* A host may only reference entities in its own vault, or the shared bundle
 * carries dangling ids for every other member. The pickers only offer in-vault
 * options, so this catches a selection that has since left the vault or been
 * deleted — the backend rejects it either way, this names the field. */
function crossVaultErrors(state: FormState, refs: References): FieldErrors {
  const missing = <T extends { id: string }>(id: string, options: T[]) =>
    Boolean(id) && !options.some((option) => option.id === id);
  const errors: FieldErrors = {};
  if (missing(state.groupId, refs.groups)) {
    errors.groupId = "That group is not in this host's vault.";
  }
  if (missing(state.keyId, refs.keyReferences)) {
    errors.keyId = "That key is not in this host's vault.";
  }
  if (missing(state.identityId, refs.identities)) {
    errors.identityId = "That identity is not in this host's vault.";
  }
  if (missing(state.proxyJumpHostId, refs.hosts)) {
    errors.proxyJumpHostId = "That jump host is not in this host's vault.";
  }
  return errors;
}

function validate(state: FormState): FieldErrors {
  const errors: FieldErrors = {};
  if (!state.name.trim()) errors.name = "Name is required.";

  const hostname = state.hostname.trim();
  if (!hostname) errors.hostname = "Hostname is required.";
  else if (/\s/.test(hostname)) errors.hostname = "Hostname cannot contain whitespace.";
  else if (hostname.startsWith("-")) errors.hostname = "Hostname cannot start with '-'.";

  const username = state.username.trim();
  if (!state.identityId && username) {
    if (/\s/.test(username)) errors.username = "Username cannot contain whitespace.";
    else if (username.startsWith("-")) errors.username = "Username cannot start with '-'.";
  }

  const port = Number(state.port);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    errors.port = "Port must be between 1 and 65535.";
  }

  if (!state.identityId && state.authenticationType === "key" && !state.keyId) {
    errors.keyId = "Select a key reference for key authentication.";
  }

  const moshServerPath = state.moshServerPath.trim();
  if (moshServerPath && !/^[A-Za-z0-9._/~+-]+$/.test(moshServerPath)) {
    errors.moshServerPath =
      "Path may only contain letters, digits, and / . _ - + ~ (no spaces or quotes).";
  } else if (moshServerPath.startsWith("-")) {
    errors.moshServerPath = "Path cannot start with '-'.";
  }
  const moshPortRange = state.moshPortRange.trim();
  if (moshPortRange) {
    const match = /^(\d{1,5})(?:-(\d{1,5}))?$/.exec(moshPortRange);
    const low = match ? Number(match[1]) : NaN;
    const high = match?.[2] !== undefined ? Number(match[2]) : low;
    if (!match || low < 1 || high > 65535 || low > high) {
      errors.moshPortRange = "Use a port or low-high range between 1 and 65535 (e.g. 60000-61000).";
    }
  }
  return errors;
}

function toInput(state: FormState): HostInput {
  const env = state.env.filter((row) => row.key.trim());
  const usesIdentity = Boolean(state.identityId);
  return {
    name: state.name.trim(),
    hostname: state.hostname.trim(),
    port: Number(state.port),
    username: usesIdentity ? null : state.username.trim() || null,
    groupId: state.groupId || null,
    authenticationType: usesIdentity ? "interactive" : state.authenticationType,
    keyId: !usesIdentity && state.authenticationType === "key" ? state.keyId || null : null,
    identityId: state.identityId || null,
    proxyJumpHostId: state.proxyJumpHostId || null,
    startupCommand: state.startupCommand.trim() || null,
    workingDirectory: state.workingDirectory.trim() || null,
    environment: env.length
      ? Object.fromEntries(env.map((row) => [row.key.trim(), row.value]))
      : null,
    tags: state.tags
      .split(",")
      .map((t) => t.trim())
      .filter((t) => t.length > 0),
    favorite: state.favorite,
    tabColor: state.tabColor || null,
    transport: state.transport,
    moshServerPath: state.moshServerPath.trim() || null,
    moshPortRange: state.moshPortRange.trim() || null,
  };
}

function groupLabel(group: HostGroup, groups: HostGroup[]): string {
  const parent = group.parentId
    ? groups.find((candidate) => candidate.id === group.parentId)
    : null;
  return parent ? `${parent.name} / ${group.name}` : group.name;
}

/** Connection options tucked under "Show more". A host that already uses any of
 * them opens expanded, so editing never hides a setting that changes how it
 * connects. Favorite and tab color are visible in the host list and tab strip. */
function hasAdvancedSettings(state: FormState): boolean {
  return Boolean(
    state.proxyJumpHostId ||
    state.transport !== "ssh" ||
    state.startupCommand ||
    state.workingDirectory ||
    state.env.length > 0,
  );
}

function EditorSection({ title, children }: { title?: string; children: React.ReactNode }) {
  return (
    <section className="space-y-2.5 rounded-lg bg-raised/60 p-3">
      {title && <h3 className="text-xs font-semibold text-foreground">{title}</h3>}
      {children}
    </section>
  );
}

const controlFrame =
  "flex items-center gap-2 rounded-md border bg-background px-2.5 focus-within:ring-1";
const controlTone = (error: string | undefined) =>
  error
    ? "border-danger focus-within:border-danger focus-within:ring-danger"
    : "border-border focus-within:border-accent focus-within:ring-accent";

function FieldMessages({
  id,
  error,
  hint,
}: {
  id: string;
  error?: string;
  hint?: React.ReactNode;
}) {
  return (
    <>
      {hint && <div className="mt-1">{hint}</div>}
      {error && (
        <p id={`${id}-error`} className="mt-1 text-xs text-danger">
          {error}
        </p>
      )}
    </>
  );
}

/** A single-line input whose placeholder doubles as its visible label; the
 * label stays available to assistive tech through aria-label. */
function IconInput({
  label,
  icon: Icon,
  value,
  onChange,
  placeholder,
  mono,
  error,
  hint,
}: {
  label: string;
  icon?: LucideIcon;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  mono?: boolean;
  error?: string;
  hint?: React.ReactNode;
}) {
  const id = useId();
  return (
    <div>
      <div className={cn(controlFrame, controlTone(error))}>
        {Icon && <Icon size={14} aria-hidden="true" className="shrink-0 text-muted" />}
        <input
          id={id}
          aria-label={label}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder ?? label}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? `${id}-error` : undefined}
          className={cn(
            "min-w-0 flex-1 bg-transparent py-2 text-sm outline-none placeholder:text-muted/80",
            mono && "font-mono placeholder:font-sans",
          )}
        />
      </div>
      <FieldMessages id={id} error={error} hint={hint} />
    </div>
  );
}

function IconSelect({
  label,
  icon: Icon,
  value,
  onChange,
  error,
  hint,
  children,
}: {
  label: string;
  icon: LucideIcon;
  value: string;
  onChange: (value: string) => void;
  error?: string;
  hint?: React.ReactNode;
  children: React.ReactNode;
}) {
  const id = useId();
  return (
    <div>
      <div className={cn(controlFrame, controlTone(error))}>
        <Icon size={14} aria-hidden="true" className="shrink-0 text-muted" />
        <select
          id={id}
          aria-label={label}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? `${id}-error` : undefined}
          className={cn(
            "min-w-0 flex-1 bg-transparent py-2 text-sm outline-none",
            !value && "text-muted",
          )}
        >
          {children}
        </select>
      </div>
      <FieldMessages id={id} error={error} hint={hint} />
    </div>
  );
}

export function HostEditorDialog({
  open,
  onOpenChange,
  host,
  groups,
  keyReferences,
  identities,
  hosts,
  onManageKeys,
  onConnect,
  initialGroupId = null,
  vaultId,
  vaultName,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  host: Host | null;
  groups: HostGroup[];
  keyReferences: KeyReference[];
  identities: Identity[];
  hosts: Host[];
  onManageKeys: () => void;
  /** Opens a session to the host once it has been saved. */
  onConnect: (host: Host) => void;
  initialGroupId?: string | null;
  /** Vault a new host lands in; omitted means the backend's default (personal).
   * Ignored when editing — a host never changes vault. */
  vaultId?: string;
  /** Shown so it is never a surprise which vault a host belongs to — a shared
   * one puts it in front of everyone who has joined. */
  vaultName?: string;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const previousFocus = document.activeElement;
    panelRef.current?.querySelector<HTMLInputElement>("input")?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !document.querySelector('[role="dialog"]')) {
        event.preventDefault();
        onOpenChange(false);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected) previousFocus.focus();
    };
  }, [open, onOpenChange]);
  const invalidate = useInvalidateHosts();
  const [state, setState] = useState<FormState>(() => initialState(host, initialGroupId));
  const [showErrors, setShowErrors] = useState(false);
  const [showMore, setShowMore] = useState(false);

  // Re-seed the form whenever the dialog opens for a different host.
  useEffect(() => {
    if (open) {
      const seeded = initialState(host, initialGroupId);
      setState(seeded);
      setShowErrors(false);
      setShowMore(hasAdvancedSettings(seeded));
    }
  }, [open, host, initialGroupId]);

  const proxyOptions = useMemo(() => hosts.filter((h) => h.id !== host?.id), [hosts, host?.id]);

  /* What the group selected in the form would supply for each field. Resolved
   * by the backend against the same rules the connection path runs, so these
   * hints can never drift from what actually happens on connect. It follows the
   * group picker rather than the saved group, so switching groups updates the
   * hints before anything is saved. */
  const inherited = useQuery({
    queryKey: ["host-inherited-defaults", state.groupId || null],
    queryFn: () => groupInheritedDefaults(state.groupId || null),
    enabled: open,
  });

  const inheritedFor = (field: InheritableField) => {
    const data = inherited.data;
    if (!data) return null;
    const groupId = inheritedGroupId(data.origins[field]);
    if (!groupId) return null;
    const from = groups.find((g) => g.id === groupId)?.name ?? "its group";
    const raw = data.host[field];
    let value = typeof raw === "string" ? raw : "";
    if (field === "identityId") value = identities.find((i) => i.id === raw)?.name ?? value;
    if (field === "proxyJumpHostId") value = hosts.find((h) => h.id === raw)?.name ?? value;
    if (field === "transport") {
      value = TRANSPORT_OPTIONS.find((option) => option.value === raw)?.label ?? value;
    }
    return { from, value };
  };

  /** A muted label on an inherited field, and a reset affordance once the host
   * overrides it. Nothing renders when the group supplies no default. */
  const hint = (field: InheritableField, overridden: boolean, reset: () => void) => {
    const info = inheritedFor(field);
    if (!info) return undefined;
    return overridden ? (
      <span className="text-[11px] text-muted/80">
        Overrides {info.from} ·{" "}
        <button type="button" onClick={reset} className="text-accent hover:underline">
          Reset
        </button>
      </span>
    ) : (
      <span className="text-[11px] text-muted/80">
        Inherited from {info.from}
        {info.value ? `: ${info.value}` : ""}
      </span>
    );
  };

  /** Environment variables merge per name instead of replacing wholesale, so
   * the group's variables apply alongside the host's own. */
  const inheritedEnv = Object.keys(inherited.data?.origins.environment ?? {});

  const errors = useMemo(
    () => ({
      ...validate(state),
      ...crossVaultErrors(state, {
        groups,
        keyReferences,
        identities,
        hosts: proxyOptions,
      }),
    }),
    [state, groups, keyReferences, identities, proxyOptions],
  );
  const hasErrors = Object.keys(errors).length > 0;

  const save = useMutation({
    mutationFn: ({ input }: { input: HostInput; connect: boolean }) =>
      host ? updateHost(host.id, input) : createHost(input),
    onSuccess: (saved, { connect }) => {
      invalidate();
      onOpenChange(false);
      if (connect) onConnect(saved);
    },
  });

  const patch = (partial: Partial<FormState>) => setState((prev) => ({ ...prev, ...partial }));

  const submit = (connect: boolean) => {
    setShowErrors(true);
    if (hasErrors) {
      // An invalid advanced field must not stay hidden behind "Show more".
      if (errors.proxyJumpHostId || errors.moshServerPath || errors.moshPortRange) {
        setShowMore(true);
      }
      // The errors render on the next commit; move focus to the first one so a
      // keyboard user lands on what needs fixing instead of on the page body.
      requestAnimationFrame(() => {
        panelRef.current?.querySelector<HTMLElement>('[aria-invalid="true"]')?.focus();
      });
      return;
    }
    const input = host ? toInput(state) : { ...toInput(state), vaultId };
    save.mutate({ input, connect });
  };

  const backendError = save.isError ? parseLumaError(save.error) : null;
  // Surface backend invalid-input under the most likely field, else globally.
  const err = (field: keyof FieldErrors) => (showErrors ? errors[field] : undefined);

  if (!open) return null;

  const address = state.hostname.trim();
  const osId = host?.osId && host.osId !== "unknown" ? host.osId : null;

  return (
    <div ref={panelRef} className="absolute inset-y-0 right-0 z-20 flex">
      <InspectorShell
        onClose={() => onOpenChange(false)}
        title={host ? "Host details" : "New host"}
        subtitle={vaultName ?? (host ? host.name : "Save an SSH connection for quick access.")}
        actions={
          <button
            type="button"
            onClick={() => submit(false)}
            disabled={save.isPending}
            aria-label={host ? "Save changes" : "Create host"}
            title={host ? "Save changes" : "Create host"}
            className="mr-1 rounded p-1 text-muted hover:bg-raised hover:text-foreground disabled:opacity-50"
          >
            <Check size={16} />
          </button>
        }
        footer={
          <button
            type="button"
            onClick={() => submit(true)}
            disabled={save.isPending}
            className="w-full rounded-md bg-accent px-3 py-2 text-sm font-medium text-accent-foreground hover:brightness-110 disabled:opacity-50"
          >
            {host ? "Save and connect" : "Create and connect"}
          </button>
        }
      >
        <div className="space-y-3">
          <EditorSection title="Address">
            <div className="flex items-start gap-2.5">
              <span
                aria-hidden="true"
                className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-accent/15 text-accent"
              >
                {osId ? (
                  <DistroIcon osId={osId} size={20} label={host?.osPrettyName ?? undefined} />
                ) : (
                  <Server size={17} />
                )}
              </span>
              <div className="min-w-0 flex-1">
                <IconInput
                  label="Hostname or IP address"
                  mono
                  value={state.hostname}
                  onChange={(v) => patch({ hostname: v })}
                  placeholder="Hostname or IP, e.g. example.com"
                  error={err("hostname")}
                />
              </div>
            </div>
          </EditorSection>

          <EditorSection title="General">
            <IconInput
              label="Name"
              value={state.name}
              onChange={(v) => patch({ name: v })}
              placeholder={address ? `Name, e.g. ${address}` : "Name, e.g. Production web"}
              error={err("name")}
            />
            <IconSelect
              label="Group"
              icon={Folder}
              value={state.groupId}
              onChange={(v) => patch({ groupId: v })}
              error={err("groupId")}
            >
              <option value="">No group</option>
              {groups.map((g) => (
                <option key={g.id} value={g.id}>
                  {groupLabel(g, groups)}
                </option>
              ))}
            </IconSelect>
            <IconInput
              label="Tags"
              icon={Tag}
              value={state.tags}
              onChange={(v) => patch({ tags: v })}
              placeholder="Tags, comma separated"
            />
          </EditorSection>

          <EditorSection>
            <div>
              <div className="flex items-center gap-2 text-xs font-semibold text-foreground">
                <span>SSH on</span>
                <input
                  aria-label="Port"
                  type="number"
                  min={1}
                  max={65535}
                  value={state.port}
                  onChange={(e) => patch({ port: e.target.value })}
                  placeholder="22"
                  aria-invalid={err("port") ? true : undefined}
                  className={cn(
                    "w-20 rounded-md border bg-background px-2 py-1 font-mono text-sm font-normal outline-none focus:ring-1",
                    err("port")
                      ? "border-danger focus:border-danger focus:ring-danger"
                      : "border-border focus:border-accent focus:ring-accent",
                  )}
                />
                <span>port</span>
              </div>
              {err("port") && <p className="mt-1 text-xs text-danger">{err("port")}</p>}
            </div>

            <div className="space-y-2.5 border-t border-border pt-3">
              <h3 className="text-xs font-semibold text-foreground">Credentials</h3>
              <IconSelect
                label="Identity"
                icon={Contact}
                value={state.identityId}
                onChange={(v) => patch({ identityId: v })}
                error={err("identityId")}
                hint={hint("identityId", Boolean(state.identityId), () =>
                  patch({ identityId: "" }),
                )}
              >
                <option value="">Host-specific credentials</option>
                {identities.map((identity) => (
                  <option key={identity.id} value={identity.id}>
                    {identity.name} ({identity.username})
                  </option>
                ))}
              </IconSelect>
              {!state.identityId && (
                <>
                  <IconInput
                    label="Username"
                    icon={UserRound}
                    mono
                    value={state.username}
                    onChange={(v) => patch({ username: v })}
                    placeholder={inheritedFor("username")?.value || "Username, e.g. root"}
                    error={err("username")}
                    hint={hint("username", state.username !== "", () => patch({ username: "" }))}
                  />
                  <IconSelect
                    label="Authentication"
                    icon={LockKeyhole}
                    value={state.authenticationType}
                    onChange={(v) => patch({ authenticationType: v as AuthenticationType })}
                  >
                    {AUTH_OPTIONS.map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {opt.label}
                      </option>
                    ))}
                  </IconSelect>
                  {state.authenticationType === "key" && (
                    <div>
                      <IconSelect
                        label="Key reference"
                        icon={KeyRound}
                        value={state.keyId}
                        onChange={(v) => patch({ keyId: v })}
                        error={err("keyId")}
                      >
                        <option value="">Select a key…</option>
                        {keyReferences.map((k) => (
                          <option key={k.id} value={k.id}>
                            {k.name}
                          </option>
                        ))}
                      </IconSelect>
                      <button
                        type="button"
                        onClick={onManageKeys}
                        className="mt-1.5 flex items-center gap-1 text-xs text-accent hover:underline"
                      >
                        <KeyRound size={11} /> Manage keys
                      </button>
                    </div>
                  )}
                  {(state.authenticationType === "password" ||
                    state.authenticationType === "interactive") && (
                    <p className="text-xs text-muted">
                      You will be prompted for the password in the terminal. Luma does not store SSH
                      passwords.
                    </p>
                  )}
                </>
              )}
            </div>

            <div className="border-t border-border pt-2">
              <button
                type="button"
                onClick={() => setShowMore((v) => !v)}
                aria-expanded={showMore}
                className="flex items-center gap-1 rounded py-1 text-xs font-medium text-muted hover:text-foreground"
              >
                {showMore ? "Show less" : "Show more"}
                <ChevronDown
                  size={13}
                  aria-hidden="true"
                  className={cn("transition-transform", showMore && "rotate-180")}
                />
              </button>
              {showMore && (
                <div className="mt-2 space-y-2.5">
                  <IconSelect
                    label="Proxy jump"
                    icon={Waypoints}
                    value={state.proxyJumpHostId}
                    onChange={(v) => patch({ proxyJumpHostId: v })}
                    error={err("proxyJumpHostId")}
                    hint={hint("proxyJumpHostId", Boolean(state.proxyJumpHostId), () =>
                      patch({ proxyJumpHostId: "" }),
                    )}
                  >
                    <option value="">No proxy jump</option>
                    {proxyOptions.map((h) => (
                      <option key={h.id} value={h.id}>
                        Jump via {h.name}
                      </option>
                    ))}
                  </IconSelect>
                  <IconSelect
                    label="Transport"
                    icon={Cable}
                    value={state.transport}
                    onChange={(v) => patch({ transport: v as TransportType })}
                    hint={hint("transport", state.transport !== "ssh", () =>
                      patch({ transport: "ssh" }),
                    )}
                  >
                    {TRANSPORT_OPTIONS.map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {opt.label}
                      </option>
                    ))}
                  </IconSelect>
                  {state.transport !== "ssh" && (
                    <>
                      <IconInput
                        label="mosh-server path"
                        mono
                        value={state.moshServerPath}
                        onChange={(v) => patch({ moshServerPath: v })}
                        placeholder={
                          inheritedFor("moshServerPath")?.value || "mosh-server path (optional)"
                        }
                        error={err("moshServerPath")}
                        hint={hint("moshServerPath", state.moshServerPath !== "", () =>
                          patch({ moshServerPath: "" }),
                        )}
                      />
                      <IconInput
                        label="Mosh UDP port range"
                        mono
                        value={state.moshPortRange}
                        onChange={(v) => patch({ moshPortRange: v })}
                        placeholder={
                          inheritedFor("moshPortRange")?.value || "Mosh UDP ports, e.g. 60000-61000"
                        }
                        error={err("moshPortRange")}
                        hint={hint("moshPortRange", state.moshPortRange !== "", () =>
                          patch({ moshPortRange: "" }),
                        )}
                      />
                    </>
                  )}
                  <IconInput
                    label="Startup command"
                    icon={SquareTerminal}
                    mono
                    value={state.startupCommand}
                    onChange={(v) => patch({ startupCommand: v })}
                    placeholder={
                      inheritedFor("startupCommand")?.value || "Startup command, e.g. tmux a"
                    }
                    hint={hint("startupCommand", state.startupCommand !== "", () =>
                      patch({ startupCommand: "" }),
                    )}
                  />
                  <IconInput
                    label="Remote working directory"
                    icon={FolderOpen}
                    mono
                    value={state.workingDirectory}
                    onChange={(v) => patch({ workingDirectory: v })}
                    placeholder={
                      inheritedFor("workingDirectory")?.value || "Working directory, e.g. /var/www"
                    }
                    hint={hint("workingDirectory", state.workingDirectory !== "", () =>
                      patch({ workingDirectory: "" }),
                    )}
                  />
                  <EnvironmentEditor
                    rows={state.env}
                    onChange={(env) => patch({ env })}
                    hint={
                      inheritedEnv.length > 0 ? (
                        <span className="text-[11px] text-muted/80">
                          +{inheritedEnv.length} inherited ({inheritedEnv.join(", ")})
                        </span>
                      ) : undefined
                    }
                  />
                  <TabColorField
                    value={state.tabColor}
                    onChange={(tabColor) => patch({ tabColor })}
                    hint={hint("tabColor", state.tabColor !== "", () => patch({ tabColor: "" }))}
                  />
                  <CheckboxField
                    label="Favorite"
                    checked={state.favorite}
                    onChange={(v) => patch({ favorite: v })}
                  />
                </div>
              )}
            </div>
          </EditorSection>

          {backendError && (
            <p className="text-xs text-danger">
              {backendError.category === "invalid-input"
                ? backendError.message
                : `Could not save host: ${backendError.message}`}
            </p>
          )}
        </div>
      </InspectorShell>
    </div>
  );
}
