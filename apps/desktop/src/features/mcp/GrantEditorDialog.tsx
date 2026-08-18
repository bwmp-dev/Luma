import { useEffect, useMemo, useState } from "react";
import { AlertTriangle, Check, Copy, Search } from "lucide-react";
import { Modal } from "../../components/Modal";
import { useHosts } from "../../hooks/useHosts";
import { useMcpExecutablePath, useMcpGrantMutations } from "../../hooks/useMcp";
import { buildClientConfig, type McpGrant } from "../../lib/mcp";
import { parseLumaError } from "../../lib/hosts";
import { cn } from "../../lib/utils";

/**
 * Create or edit a grant.
 *
 * On create the dialog switches to a one-time reveal: the token exists only in
 * that response, so closing before copying means issuing a new grant.
 */
export function GrantEditorDialog({
  open,
  onOpenChange,
  grant,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Omitted when creating. */
  grant?: McpGrant;
}) {
  const { data: hosts } = useHosts();
  const { create, update } = useMcpGrantMutations();
  const { data: executable } = useMcpExecutablePath();

  const [label, setLabel] = useState("");
  const [hostIds, setHostIds] = useState<string[]>([]);
  const [requireApproval, setRequireApproval] = useState(false);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [issuedToken, setIssuedToken] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  // Reset per opening rather than per mount: the dialog stays mounted.
  useEffect(() => {
    if (!open) return;
    setLabel(grant?.label ?? "");
    setHostIds(grant?.hostIds ?? []);
    setRequireApproval(grant?.requireApproval ?? false);
    setFilter("");
    setError(null);
    setIssuedToken(null);
    setCopied(false);
  }, [open, grant]);

  const visibleHosts = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const all = hosts ?? [];
    if (!needle) return all;
    return all.filter(
      (host) =>
        host.name.toLowerCase().includes(needle) ||
        host.hostname.toLowerCase().includes(needle),
    );
  }, [hosts, filter]);

  const busy = create.isPending || update.isPending;
  const canSave = label.trim().length > 0 && !busy;

  const toggleHost = (id: string) =>
    setHostIds((current) =>
      current.includes(id)
        ? current.filter((hostId) => hostId !== id)
        : [...current, id],
    );

  const save = async () => {
    setError(null);
    try {
      if (grant) {
        await update.mutateAsync({
          id: grant.id,
          label: label.trim(),
          hostIds,
          requireApproval,
        });
        onOpenChange(false);
      } else {
        const created = await create.mutateAsync({
          label: label.trim(),
          hostIds,
          requireApproval,
        });
        setIssuedToken(created.token);
      }
    } catch (cause) {
      setError(parseLumaError(cause).message);
    }
  };

  const config =
    issuedToken && executable ? buildClientConfig(executable, issuedToken) : null;

  const copyConfig = async () => {
    if (!config) return;
    await navigator.clipboard.writeText(config);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 2000);
  };

  if (issuedToken) {
    return (
      <Modal
        open={open}
        onOpenChange={onOpenChange}
        title="Grant created"
        description="Copy this configuration now — the token is not stored and cannot be shown again."
        size="lg"
        footer={
          <button
            type="button"
            onClick={() => onOpenChange(false)}
            className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-foreground"
          >
            Done
          </button>
        }
      >
        <div className="space-y-3">
          <div className="flex items-start gap-1.5 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-xs text-warning">
            <AlertTriangle size={13} className="mt-0.5 shrink-0" />
            <span>
              Luma stores only a hash of this token. If you lose it, delete this
              grant and create another.
            </span>
          </div>

          <div>
            <div className="mb-1.5 flex items-center justify-between gap-2">
              <p className="text-xs font-medium text-foreground">
                Add to your agent&apos;s MCP configuration
              </p>
              <button
                type="button"
                onClick={() => void copyConfig()}
                disabled={!config}
                className="flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1 text-xs text-muted hover:text-foreground disabled:opacity-60"
              >
                {copied ? <Check size={12} className="text-accent" /> : <Copy size={12} />}
                {copied ? "Copied" : "Copy"}
              </button>
            </div>
            <pre className="max-h-64 overflow-auto rounded-md border border-border bg-background px-3 py-2.5 font-mono text-xs text-foreground">
              {config ?? "Resolving Luma's path…"}
            </pre>
          </div>
        </div>
      </Modal>
    );
  }

  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title={grant ? "Edit grant" : "New grant"}
      description="An agent holding this grant's token can reach the hosts you select. It never sees your keys or passwords."
      size="lg"
      footer={
        <>
          <button
            type="button"
            onClick={() => onOpenChange(false)}
            className="rounded-md border border-border px-3 py-1.5 text-sm text-muted hover:text-foreground"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() => void save()}
            disabled={!canSave}
            className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-foreground disabled:opacity-60"
          >
            {grant ? "Save changes" : "Create grant"}
          </button>
        </>
      }
    >
      <div className="space-y-4">
        <label className="block">
          <span className="text-xs font-medium text-foreground">Name</span>
          <input
            value={label}
            onChange={(event) => setLabel(event.target.value)}
            placeholder="Claude Code"
            className="mt-1 w-full rounded-md border border-border bg-background px-2.5 py-1.5 text-sm outline-none focus:border-accent"
          />
          <span className="mt-1 block text-xs text-muted">
            Shown in approval prompts and the activity list.
          </span>
        </label>

        <div>
          <div className="mb-1.5 flex items-center justify-between gap-2">
            <span className="text-xs font-medium text-foreground">
              Hosts this agent may reach
            </span>
            <span className="text-xs text-muted">{hostIds.length} selected</span>
          </div>

          {(hosts?.length ?? 0) > 8 && (
            <div className="relative mb-1.5">
              <Search
                size={13}
                className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-muted"
              />
              <input
                value={filter}
                onChange={(event) => setFilter(event.target.value)}
                placeholder="Filter hosts"
                aria-label="Filter hosts"
                className="w-full rounded-md border border-border bg-background py-1.5 pl-7 pr-2.5 text-sm outline-none focus:border-accent"
              />
            </div>
          )}

          <div className="max-h-56 overflow-y-auto rounded-md border border-border">
            {visibleHosts.length === 0 ? (
              <p className="px-3 py-4 text-center text-xs text-muted">
                {hosts?.length ? "No hosts match that filter." : "No saved hosts yet."}
              </p>
            ) : (
              visibleHosts.map((host) => {
                const selected = hostIds.includes(host.id);
                return (
                  <label
                    key={host.id}
                    className={cn(
                      "flex cursor-pointer items-center gap-2.5 border-b border-border px-3 py-2 last:border-b-0",
                      selected ? "bg-accent/5" : "hover:bg-raised",
                    )}
                  >
                    <input
                      type="checkbox"
                      checked={selected}
                      onChange={() => toggleHost(host.id)}
                      className="accent-accent"
                    />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-sm text-foreground">
                        {host.name}
                      </span>
                      <span className="block truncate text-xs text-muted">
                        {host.username ? `${host.username}@` : ""}
                        {host.hostname}
                      </span>
                    </span>
                  </label>
                );
              })
            )}
          </div>
          {hostIds.length === 0 && (
            <p className="mt-1 text-xs text-muted">
              With no hosts selected the agent can still read panes you share,
              but cannot run commands.
            </p>
          )}
        </div>

        <label className="flex cursor-pointer items-start gap-2.5 rounded-md border border-border px-3 py-2.5">
          <input
            type="checkbox"
            checked={requireApproval}
            onChange={(event) => setRequireApproval(event.target.checked)}
            className="mt-0.5 accent-accent"
          />
          <span className="min-w-0">
            <span className="block text-sm text-foreground">Ask before each action</span>
            <span className="mt-0.5 block text-xs text-muted">
              Every command and keystroke waits for you to allow it. Unanswered
              prompts are denied after two minutes.
            </span>
          </span>
        </label>

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
