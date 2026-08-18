import { useState } from "react";
import {
  Bot,
  KeyRound,
  MonitorPlay,
  Pencil,
  Plus,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { formatRelativeTime } from "../../lib/sync";
import {
  useMcpActivity,
  useMcpGrantMutations,
  useMcpGrants,
  useMcpStatus,
  usePaneShareMutations,
  useSharedPanes,
} from "../../hooks/useMcp";
import type { McpGrant } from "../../lib/mcp";
import { GrantEditorDialog } from "./GrantEditorDialog";

/**
 * Settings "Agents" section: grants, panes currently shared, and recent agent
 * activity.
 *
 * The endpoint has no separate on/off switch — it listens exactly when a grant
 * exists, so creating the first grant is the opt-in and deleting the last one
 * closes it.
 */
export function McpSection() {
  const { data: status } = useMcpStatus();
  const { data: grants } = useMcpGrants();
  const { data: sharedPanes } = useSharedPanes();
  const { data: activity } = useMcpActivity(true);
  const { remove } = useMcpGrantMutations();
  const { unshare } = usePaneShareMutations();

  const [editing, setEditing] = useState<McpGrant | undefined>();
  const [editorOpen, setEditorOpen] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<McpGrant | null>(null);

  const openEditor = (grant?: McpGrant) => {
    setEditing(grant);
    setEditorOpen(true);
  };

  return (
    <div className="space-y-6">
      <section className="space-y-3">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <h3 className="text-sm font-medium text-foreground">Grants</h3>
            <p className="mt-0.5 text-xs text-muted">
              {status?.running
                ? `Listening on 127.0.0.1:${status.port ?? "—"} for local agents.`
                : "Create a grant to let an agent work through Luma. Nothing listens until you do."}
            </p>
          </div>
          <button
            type="button"
            onClick={() => openEditor()}
            className="flex shrink-0 items-center gap-1.5 rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-foreground"
          >
            <Plus size={14} /> New grant
          </button>
        </div>

        {grants?.length ? (
          <ul className="divide-y divide-border rounded-lg border border-border">
            {grants.map((grant) => (
              <li key={grant.id} className="flex items-center gap-3 px-3 py-2.5">
                <Bot size={16} className="shrink-0 text-muted" />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <p className="truncate text-sm font-medium text-foreground">
                      {grant.label}
                    </p>
                    {grant.requireApproval && (
                      <span className="flex shrink-0 items-center gap-1 rounded border border-border px-1.5 py-0.5 text-[11px] text-muted">
                        <ShieldCheck size={10} /> Asks first
                      </span>
                    )}
                  </div>
                  <p className="mt-0.5 truncate text-xs text-muted">
                    <span className="font-mono">{grant.tokenPrefix}…</span> ·{" "}
                    {grant.hostIds.length === 1
                      ? "1 host"
                      : `${grant.hostIds.length} hosts`}{" "}
                    ·{" "}
                    {grant.lastUsedAt
                      ? `used ${formatRelativeTime(grant.lastUsedAt * 1000)}`
                      : "never used"}
                  </p>
                </div>
                <button
                  type="button"
                  aria-label={`Edit ${grant.label}`}
                  onClick={() => openEditor(grant)}
                  className="shrink-0 rounded-md p-1.5 text-muted hover:bg-raised hover:text-foreground"
                >
                  <Pencil size={14} />
                </button>
                <button
                  type="button"
                  aria-label={`Delete ${grant.label}`}
                  onClick={() => setPendingDelete(grant)}
                  className="shrink-0 rounded-md p-1.5 text-muted hover:bg-raised hover:text-danger"
                >
                  <Trash2 size={14} />
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <div className="rounded-lg border border-dashed border-border px-3 py-6 text-center">
            <KeyRound size={18} className="mx-auto text-muted" />
            <p className="mt-2 text-sm text-foreground">No grants yet</p>
            <p className="mx-auto mt-1 max-w-sm text-xs text-muted">
              A grant is a token scoped to specific hosts. The agent runs
              commands through Luma, so your keys and passwords stay here.
            </p>
          </div>
        )}
      </section>

      {sharedPanes?.length ? (
        <section className="space-y-2">
          <div>
            <h3 className="text-sm font-medium text-foreground">Shared panes</h3>
            <p className="mt-0.5 text-xs text-muted">
              Terminals an agent can currently read and type into. Sharing ends
              when the tab closes.
            </p>
          </div>
          <ul className="divide-y divide-border rounded-lg border border-border">
            {sharedPanes.map((pane) => {
              const grant = grants?.find((entry) => entry.id === pane.grantId);
              return (
                <li
                  key={pane.sessionId}
                  className="flex items-center gap-3 px-3 py-2.5"
                >
                  <MonitorPlay size={16} className="shrink-0 text-accent" />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm text-foreground">{pane.title}</p>
                    <p className="mt-0.5 truncate text-xs text-muted">
                      Shared with {grant?.label ?? "a deleted grant"}
                    </p>
                  </div>
                  <button
                    type="button"
                    onClick={() => unshare.mutate(pane.sessionId)}
                    className="shrink-0 rounded-md border border-border px-2.5 py-1 text-xs text-muted hover:text-foreground"
                  >
                    Stop sharing
                  </button>
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}

      <section className="space-y-2">
        <div>
          <h3 className="text-sm font-medium text-foreground">Recent activity</h3>
          <p className="mt-0.5 text-xs text-muted">
            What agents have done. Luma records the shape of each action, never
            the command text or terminal contents.
          </p>
        </div>
        {activity?.length ? (
          <ul className="max-h-72 divide-y divide-border overflow-y-auto rounded-lg border border-border">
            {activity.map((entry) => (
              <li key={entry.id} className="px-3 py-2">
                <div className="flex items-baseline justify-between gap-3">
                  <p className="min-w-0 truncate text-sm text-foreground">
                    <span className="text-muted">{entry.grantLabel}</span>{" "}
                    {entry.action === "run-command" ? "ran a command on" : "typed into"}{" "}
                    {entry.target}
                  </p>
                  <span className="shrink-0 text-xs text-muted">
                    {formatRelativeTime(entry.at * 1000)}
                  </span>
                </div>
                <p className="mt-0.5 text-xs text-muted">
                  {entry.error ? (
                    <span className="text-danger">{entry.error}</span>
                  ) : (
                    <>
                      {entry.inputBytes} bytes sent
                      {entry.exitCode !== null && ` · exit ${entry.exitCode}`}
                      {entry.durationMs !== null && ` · ${entry.durationMs} ms`}
                    </>
                  )}
                </p>
              </li>
            ))}
          </ul>
        ) : (
          <p className="rounded-lg border border-dashed border-border px-3 py-4 text-center text-xs text-muted">
            Nothing yet.
          </p>
        )}
      </section>

      <GrantEditorDialog
        open={editorOpen}
        onOpenChange={setEditorOpen}
        grant={editing}
      />

      <ConfirmDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => !open && setPendingDelete(null)}
        title="Delete this grant?"
        message={
          <>
            Any agent using <strong>{pendingDelete?.label}</strong> loses access
            immediately, and its shared panes stop being readable. This cannot be
            undone — you would have to issue a new token.
          </>
        }
        confirmLabel="Delete grant"
        destructive
        busy={remove.isPending}
        onConfirm={() => {
          if (!pendingDelete) return;
          remove.mutate(pendingDelete.id, {
            onSettled: () => setPendingDelete(null),
          });
        }}
      />
    </div>
  );
}
