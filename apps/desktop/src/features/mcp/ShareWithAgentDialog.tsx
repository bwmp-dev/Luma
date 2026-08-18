import { useEffect, useState } from "react";
import { AlertTriangle, Bot } from "lucide-react";
import { Modal } from "../../components/Modal";
import { useMcpGrants, usePaneShareMutations } from "../../hooks/useMcp";
import { parseLumaError } from "../../lib/hosts";
import { useMcpShareStore } from "../../stores/mcpStore";
import { cn } from "../../lib/utils";

/**
 * Pick which grant a terminal pane is shared with.
 *
 * Sharing is per-pane and lives only in memory: it ends when the tab closes,
 * when the grant is edited or deleted, and when the keystore locks.
 */
export function ShareWithAgentDialog() {
  const target = useMcpShareStore((state) => state.target);
  const close = useMcpShareStore((state) => state.close);
  const { data: grants } = useMcpGrants();
  const { share } = usePaneShareMutations();

  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!target) return;
    setSelected(grants?.length === 1 ? grants[0].id : null);
    setError(null);
  }, [target, grants]);

  if (!target) return null;

  const confirm = () => {
    if (!selected) return;
    setError(null);
    share.mutate(
      { sessionId: target.sessionId, grantId: selected, title: target.title },
      {
        onSuccess: () => close(),
        onError: (cause) => setError(parseLumaError(cause).message),
      },
    );
  };

  return (
    <Modal
      open
      onOpenChange={(open) => !open && close()}
      title="Share this terminal with an agent"
      description={`"${target.title}" becomes readable, and the agent can type into it, until you stop sharing or close the tab.`}
      footer={
        <>
          <button
            type="button"
            onClick={close}
            className="rounded-md border border-border px-3 py-1.5 text-sm text-muted hover:text-foreground"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={confirm}
            disabled={!selected || share.isPending}
            className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-foreground disabled:opacity-60"
          >
            Share
          </button>
        </>
      }
    >
      <div className="space-y-3">
        {grants?.length ? (
          <ul className="divide-y divide-border rounded-md border border-border">
            {grants.map((grant) => (
              <li key={grant.id}>
                <label
                  className={cn(
                    "flex cursor-pointer items-center gap-2.5 px-3 py-2.5",
                    selected === grant.id ? "bg-accent/5" : "hover:bg-raised",
                  )}
                >
                  <input
                    type="radio"
                    name="mcp-grant"
                    checked={selected === grant.id}
                    onChange={() => setSelected(grant.id)}
                    className="accent-accent"
                  />
                  <Bot size={15} className="shrink-0 text-muted" />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm text-foreground">
                      {grant.label}
                    </span>
                    <span className="block truncate text-xs text-muted">
                      {grant.requireApproval
                        ? "Asks before each action"
                        : "Acts without asking"}
                    </span>
                  </span>
                </label>
              </li>
            ))}
          </ul>
        ) : (
          <p className="rounded-md border border-dashed border-border px-3 py-4 text-center text-xs text-muted">
            No grants yet. Create one in Settings → Agents first.
          </p>
        )}

        <p className="text-xs text-muted">
          The agent reads this pane as plain text with terminal formatting
          stripped, so logs and command output are legible but full-screen
          programs like vim or top are not.
        </p>

        {error && (
          <div
            role="alert"
            className="flex items-start gap-1.5 rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-xs text-danger"
          >
            <AlertTriangle size={13} className="mt-0.5 shrink-0" />
            {error}
          </div>
        )}
      </div>
    </Modal>
  );
}
