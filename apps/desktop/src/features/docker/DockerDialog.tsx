import { useEffect, useMemo, useState } from "react";
import {
  Activity,
  ArrowLeft,
  Info,
  Loader2,
  Play,
  RefreshCw,
  RotateCw,
  ScrollText,
  Square,
} from "lucide-react";
import { Modal } from "../../components/Modal";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { useDockerStore } from "../../stores/dockerStore";
import {
  allowedActions,
  LOG_TAIL_SIZES,
  stateBadgeClass,
  stateLabel,
  statFor,
  unavailableHint,
  type DockerActionName,
  type DockerContainer,
  type DockerStat,
} from "../../lib/docker";
import { cn } from "../../lib/utils";

/*
 * Agentless Docker view for one SSH host: containers grouped by Compose
 * project, with logs, inspect and the three reversible lifecycle actions.
 *
 * Two safety rules are visible in the markup:
 *  - Actions that cannot apply to a container's current state are not rendered
 *    at all (no Stop on an exited container), from `allowedActions`.
 *  - Every action that IS rendered only records an intent; the ConfirmDialog
 *    below — which names the container AND the host — is the sole path to the
 *    remote daemon. See `dockerStore.requestAction` / `confirmAction`.
 *
 * Stats are behind their own button because `docker stats --no-stream` samples
 * twice and takes seconds, which would make every refresh feel broken.
 */

const ACTION_LABEL: Record<DockerActionName, string> = {
  start: "Start",
  stop: "Stop",
  restart: "Restart",
};

export function DockerDialog({
  open,
  onOpenChange,
  hostId,
  hostLabel,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  hostId: string | null;
  hostLabel?: string;
}) {
  const list = useDockerStore((s) => s.list);
  const loading = useDockerStore((s) => s.loading);
  const error = useDockerStore((s) => s.error);
  const stats = useDockerStore((s) => s.stats);
  const statsLoaded = useDockerStore((s) => s.statsLoaded);
  const statsLoading = useDockerStore((s) => s.statsLoading);
  const statsError = useDockerStore((s) => s.statsError);
  const logs = useDockerStore((s) => s.logs);
  const inspect = useDockerStore((s) => s.inspect);
  const pending = useDockerStore((s) => s.pending);
  const actionBusy = useDockerStore((s) => s.actionBusy);
  const actionError = useDockerStore((s) => s.actionError);
  const store = useDockerStore.getState;

  useEffect(() => {
    if (!open || !hostId) return;
    useDockerStore.getState().open(hostId, hostLabel);
    void useDockerStore.getState().refresh();
  }, [open, hostId, hostLabel]);

  if (!hostId) return null;

  const hostName = hostLabel ?? "this host";
  const drilldown = logs !== null || inspect !== null;

  return (
    <>
      <Modal
        open={open}
        onOpenChange={onOpenChange}
        title={
          logs ? `Logs — ${logs.container}` : inspect ? `Inspect — ${inspect.container}` : "Docker"
        }
        description={
          drilldown
            ? `On ${hostName}`
            : hostLabel
              ? `Containers on ${hostLabel}`
              : "Containers on this host"
        }
        size="lg"
        footer={
          drilldown ? (
            <button
              type="button"
              onClick={() => {
                store().closeLogs();
                store().closeInspect();
              }}
              className="flex items-center gap-2 rounded-md border border-border px-3 py-1.5 text-sm text-foreground hover:border-accent hover:text-accent"
            >
              <ArrowLeft size={14} />
              Back to containers
            </button>
          ) : (
            <>
              <button
                type="button"
                disabled={statsLoading || list?.available !== true}
                onClick={() => void store().loadStats()}
                className="flex items-center gap-2 rounded-md border border-border px-3 py-1.5 text-sm text-foreground hover:border-accent hover:text-accent disabled:opacity-50"
              >
                {statsLoading ? (
                  <Loader2 size={14} className="animate-spin" />
                ) : (
                  <Activity size={14} />
                )}
                {statsLoaded ? "Reload stats" : "Load stats"}
              </button>
              <button
                type="button"
                disabled={loading}
                onClick={() => void store().refresh()}
                className="flex items-center gap-2 rounded-md border border-border px-3 py-1.5 text-sm text-foreground hover:border-accent hover:text-accent disabled:opacity-50"
              >
                <RefreshCw size={14} className={cn(loading && "animate-spin")} />
                Refresh
              </button>
            </>
          )
        }
      >
        {logs ? (
          <LogsView />
        ) : inspect ? (
          <InspectView />
        ) : (
          <div className="space-y-4">
            {actionError && (
              <p className="rounded-lg border border-danger/40 bg-danger/10 p-2.5 text-xs text-danger">
                {actionError}
              </p>
            )}
            {statsError && <p className="text-xs text-muted">Stats unavailable: {statsError}</p>}
            {loading ? (
              <div className="flex min-h-32 flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border text-center">
                <Loader2 size={16} className="animate-spin text-muted" />
                <p className="text-xs text-muted">Reading containers…</p>
              </div>
            ) : error ? (
              <div className="rounded-lg border border-border bg-background p-3">
                <p className="text-xs text-danger">{error}</p>
                <p className="mt-1 text-xs text-muted">Refresh once the host is reachable again.</p>
              </div>
            ) : list?.available === false ? (
              <div className="rounded-lg border border-dashed border-border p-4 text-center">
                <p className="text-sm text-foreground">
                  {list.unavailableReason ?? "Docker unavailable"}
                </p>
                <p className="mt-1 text-xs text-muted">{unavailableHint(list.unavailableReason)}</p>
              </div>
            ) : (list?.projects.length ?? 0) === 0 ? (
              <p className="rounded-lg border border-dashed border-border p-4 text-center text-xs text-muted">
                No containers on this host.
              </p>
            ) : (
              list?.projects.map((project) => (
                <section key={project.name ?? "\0ungrouped"}>
                  <h3 className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted">
                    {project.name ?? "Not in a Compose project"}
                  </h3>
                  <div className="space-y-2">
                    {project.containers.map((container) => (
                      <ContainerRow
                        key={container.id || container.name}
                        container={container}
                        stat={statsLoaded ? statFor(stats, container) : undefined}
                      />
                    ))}
                  </div>
                </section>
              ))
            )}
          </div>
        )}
      </Modal>

      {/* The mutation gate. Names the container AND the host, because the same
          container name exists on every machine running the same Compose file. */}
      <ConfirmDialog
        open={pending !== null}
        onOpenChange={(o) => !o && store().cancelAction()}
        title={pending ? `${ACTION_LABEL[pending.action]} container` : "Confirm"}
        destructive={pending?.action !== "start"}
        confirmLabel={pending ? ACTION_LABEL[pending.action] : "Confirm"}
        busy={actionBusy}
        onConfirm={() => void store().confirmAction()}
        message={
          <>
            {pending ? ACTION_LABEL[pending.action] : ""}{" "}
            <span className="font-medium text-foreground">{pending?.container.name}</span> on{" "}
            <span className="font-medium text-foreground">{hostName}</span>?
          </>
        }
      />
    </>
  );
}

function ContainerRow({
  container,
  stat,
}: {
  container: DockerContainer;
  stat: DockerStat | undefined;
}) {
  const store = useDockerStore.getState;
  const actions = allowedActions(container.state);
  return (
    <div className="rounded-lg border border-border bg-background p-3">
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate font-mono text-sm font-semibold text-foreground">
              {container.name}
            </span>
            <span
              className={cn(
                "shrink-0 rounded-full px-2 py-0.5 text-[10px]",
                stateBadgeClass(container.state),
              )}
            >
              {stateLabel(container.state)}
            </span>
            {container.service && (
              <span className="shrink-0 truncate text-[10px] text-muted">{container.service}</span>
            )}
          </div>
          <p className="mt-0.5 truncate text-xs text-muted">
            {container.image}
            {container.status ? ` · ${container.status}` : ""}
          </p>
          {container.ports && (
            <p className="mt-0.5 truncate font-mono text-[11px] text-muted">{container.ports}</p>
          )}
          {stat && (
            <p className="mt-1 text-[11px] text-muted">
              CPU{" "}
              <span className="text-foreground">
                {stat.cpuPercent === null ? "—" : `${stat.cpuPercent.toFixed(1)}%`}
              </span>
              {" · "}
              Memory <span className="text-foreground">{stat.memUsage}</span>
              {stat.memPercent !== null && ` (${stat.memPercent.toFixed(1)}%)`}
            </p>
          )}
        </div>
        <div className="flex shrink-0 flex-col items-end gap-1.5">
          <div className="flex gap-1">
            <button
              type="button"
              aria-label={`Logs for ${container.name}`}
              onClick={() => void store().openLogs(container.name, 500)}
              className="flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs text-foreground hover:border-accent hover:text-accent"
            >
              <ScrollText size={12} />
              Logs
            </button>
            <button
              type="button"
              aria-label={`Inspect ${container.name}`}
              onClick={() => void store().openInspect(container.name)}
              className="flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs text-foreground hover:border-accent hover:text-accent"
            >
              <Info size={12} />
              Inspect
            </button>
          </div>
          {actions.length > 0 && (
            <div className="flex gap-1">
              {actions.map((action) => (
                <button
                  key={action}
                  type="button"
                  aria-label={`${ACTION_LABEL[action]} ${container.name}`}
                  // Records an intent only — the ConfirmDialog sends it.
                  onClick={() => store().requestAction(container, action)}
                  className="flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs text-muted hover:border-accent hover:text-accent"
                >
                  {action === "start" ? (
                    <Play size={12} />
                  ) : action === "stop" ? (
                    <Square size={12} />
                  ) : (
                    <RotateCw size={12} />
                  )}
                  {ACTION_LABEL[action]}
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function LogsView() {
  const logs = useDockerStore((s) => s.logs);
  const [filter, setFilter] = useState("");

  // A new container or tail size means the old filter is meaningless.
  const container = logs?.container;
  useEffect(() => setFilter(""), [container]);

  const visible = useMemo(() => {
    const lines = logs?.lines.split("\n") ?? [];
    if (filter.trim().length === 0) return lines;
    const needle = filter.toLowerCase();
    return lines.filter((line) => line.toLowerCase().includes(needle));
  }, [logs?.lines, filter]);

  if (!logs) return null;

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <input
          type="text"
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
          placeholder="Filter lines…"
          aria-label="Filter log lines"
          className="min-w-40 flex-1 rounded-md border border-border bg-background px-2.5 py-1.5 text-sm text-foreground placeholder:text-muted focus:border-accent focus:outline-none"
        />
        <div className="flex overflow-hidden rounded-md border border-border">
          {LOG_TAIL_SIZES.map((size) => (
            <button
              key={size}
              type="button"
              aria-pressed={logs.tail === size}
              onClick={() => void useDockerStore.getState().openLogs(logs.container, size)}
              className={cn(
                "px-2.5 py-1.5 text-xs text-muted",
                logs.tail === size && "bg-accent/15 text-accent",
              )}
            >
              {size}
            </button>
          ))}
        </div>
      </div>
      {logs.truncated && (
        <p className="text-xs text-muted">
          The tail was larger than the transfer limit; the oldest lines were dropped.
        </p>
      )}
      {logs.loading ? (
        <div className="flex min-h-32 items-center justify-center gap-2 rounded-lg border border-dashed border-border">
          <Loader2 size={16} className="animate-spin text-muted" />
          <p className="text-xs text-muted">Reading logs…</p>
        </div>
      ) : logs.error ? (
        <p className="rounded-lg border border-border bg-background p-3 text-xs text-danger">
          {logs.error}
        </p>
      ) : (
        <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-all rounded-lg border border-border bg-background p-3 font-mono text-[11px] leading-relaxed text-foreground">
          {visible.length > 0
            ? visible.join("\n")
            : filter.trim().length > 0
              ? "No lines match the filter."
              : "No output."}
        </pre>
      )}
    </div>
  );
}

function InspectView() {
  const inspect = useDockerStore((s) => s.inspect);
  if (!inspect) return null;
  if (inspect.loading) {
    return (
      <div className="flex min-h-32 items-center justify-center gap-2 rounded-lg border border-dashed border-border">
        <Loader2 size={16} className="animate-spin text-muted" />
        <p className="text-xs text-muted">Inspecting…</p>
      </div>
    );
  }
  if (inspect.error || !inspect.data) {
    return (
      <p className="rounded-lg border border-border bg-background p-3 text-xs text-danger">
        {inspect.error ?? "The container could not be inspected."}
      </p>
    );
  }
  const data = inspect.data;
  return (
    <div className="space-y-4">
      <Section title="Overview">
        <dl className="space-y-1 text-xs">
          <Field label="Image" value={data.image} />
          <Field label="State" value={data.state} />
          {data.startedAt && <Field label="Started" value={data.startedAt} />}
          {data.command && <Field label="Command" value={data.command} />}
          {data.restartPolicy && <Field label="Restart policy" value={data.restartPolicy} />}
          <Field label="Restarts" value={String(data.restartCount)} />
          {data.networks.length > 0 && <Field label="Networks" value={data.networks.join(", ")} />}
        </dl>
      </Section>

      <Section title="Environment">
        {data.env.length === 0 ? (
          <p className="text-xs text-muted">No environment variables.</p>
        ) : (
          <>
            <ul className="space-y-1 font-mono text-[11px]">
              {data.env.map((variable) => (
                <li key={variable.key} className="flex gap-2">
                  <span className="shrink-0 text-muted">{variable.key}</span>
                  <span
                    className={cn(
                      "min-w-0 break-all",
                      variable.redacted ? "text-muted" : "text-foreground",
                    )}
                  >
                    {variable.value}
                  </span>
                </li>
              ))}
            </ul>
            {data.env.some((variable) => variable.redacted) && (
              <p className="mt-2 text-[11px] text-muted">
                Values that look like credentials are replaced on the host side and never sent to
                this app.
              </p>
            )}
          </>
        )}
      </Section>

      <Section title="Mounts">
        {data.mounts.length === 0 ? (
          <p className="text-xs text-muted">No mounts.</p>
        ) : (
          <ul className="space-y-1 font-mono text-[11px] text-foreground">
            {data.mounts.map((mount) => (
              <li key={`${mount.source}:${mount.destination}`} className="break-all">
                <span className="text-muted">{mount.kind}</span> {mount.source} →{" "}
                {mount.destination} <span className="text-muted">{mount.rw ? "rw" : "ro"}</span>
              </li>
            ))}
          </ul>
        )}
      </Section>

      <Section title="Ports">
        {data.ports.length === 0 ? (
          <p className="text-xs text-muted">No exposed ports.</p>
        ) : (
          <ul className="space-y-1 font-mono text-[11px] text-foreground">
            {data.ports.map((port) => (
              <li key={port.container}>
                {port.container}
                {port.host ? ` → ${port.host}` : " (not published)"}
              </li>
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section>
      <h3 className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted">{title}</h3>
      <div className="rounded-lg border border-border bg-background p-3">{children}</div>
    </section>
  );
}

function Field({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-2">
      <dt className="w-28 shrink-0 text-muted">{label}</dt>
      <dd className="min-w-0 break-all text-foreground">{value}</dd>
    </div>
  );
}
