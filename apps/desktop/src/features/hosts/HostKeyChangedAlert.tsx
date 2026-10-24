import { useEffect, useId, useRef } from "react";
import { ShieldAlert, ShieldCheck, X } from "lucide-react";
import type { HostKeyFingerprint } from "../../lib/ssh";

export function HostKeyChangedAlert({
  hostTitle,
  message,
  scannedKeys,
  knownKeys,
  issue,
  onClose,
  onReplace,
  onRetry,
  onOpenKnownHosts,
}: {
  hostTitle: string;
  message: string;
  /** Keys observed on the network now (what the server is presenting). */
  scannedKeys?: HostKeyFingerprint[];
  /** Previously trusted keys, shown alongside for out-of-band comparison. */
  knownKeys?: HostKeyFingerprint[];
  issue?: string;
  onClose: () => void;
  onReplace?: () => void;
  onRetry?: () => void;
  /** Open the known-hosts manager so the user can remove the stale entry. */
  onOpenKnownHosts?: () => void;
}) {
  const titleId = useId();
  const descriptionId = useId();
  const cancelRef = useRef<HTMLButtonElement>(null);
  useEffect(() => cancelRef.current?.focus({ preventScroll: true }), []);
  const hasComparison = (scannedKeys?.length ?? 0) > 0 || (knownKeys?.length ?? 0) > 0;
  return (
    <div
      role="alertdialog"
      aria-modal="true"
      aria-labelledby={titleId}
      aria-describedby={descriptionId}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          onClose();
        }
      }}
      className="absolute inset-0 z-20 flex items-start justify-center overflow-y-auto bg-background/95 p-3 backdrop-blur sm:p-6"
    >
      <div className="my-auto w-full max-w-md rounded-xl border border-danger/50 bg-surface p-5 shadow-glow">
        <div className="flex items-center gap-2 text-danger">
          <ShieldAlert size={20} className="shrink-0" />
          <h2 id={titleId} className="text-sm font-semibold">
            Host key changed for {hostTitle}
          </h2>
        </div>
        <p id={descriptionId} className="mt-3 text-sm text-muted">
          {message}
        </p>
        {issue && <p className="mt-3 text-sm text-danger">{issue}</p>}
        {hasComparison && (
          <div className="mt-3 space-y-3 rounded-lg border border-border bg-background p-3">
            <FingerprintList label="Previously trusted" keys={knownKeys} tone="muted" />
            <FingerprintList label="Presented now" keys={scannedKeys} tone="danger" />
          </div>
        )}
        <ul className="mt-3 list-disc space-y-1 pl-5 text-xs text-muted">
          <li>
            If you expected this (the server was rebuilt or its key rotated), verify the new
            fingerprint through a trusted channel.
          </li>
          <li>
            Replacing the saved key forgets the old fingerprint and continues this connection.
          </li>
          <li>If you did not expect this, do not connect — the connection may be intercepted.</li>
        </ul>
        <div className="mt-5 flex flex-wrap justify-end gap-2">
          {onOpenKnownHosts && (
            <button
              type="button"
              onClick={onOpenKnownHosts}
              className="flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm text-foreground hover:border-accent hover:text-accent"
            >
              <ShieldCheck size={14} /> Manage known hosts
            </button>
          )}
          <button
            type="button"
            ref={cancelRef}
            onClick={onClose}
            className="flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm text-foreground hover:border-accent hover:text-accent"
          >
            <X size={14} /> Cancel
          </button>
          {onReplace ? (
            <button
              type="button"
              disabled={!scannedKeys?.length}
              onClick={onReplace}
              className="flex items-center gap-1.5 rounded-md bg-danger px-3 py-1.5 text-sm font-medium text-white hover:brightness-110 disabled:opacity-40"
            >
              <ShieldCheck size={14} /> Replace key and connect
            </button>
          ) : (
            onRetry && (
              <button
                type="button"
                onClick={onRetry}
                className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-accent-foreground"
              >
                Check fingerprint again
              </button>
            )
          )}
        </div>
      </div>
    </div>
  );
}

function FingerprintList({
  label,
  keys,
  tone,
}: {
  label: string;
  keys?: HostKeyFingerprint[];
  tone: "muted" | "danger";
}) {
  return (
    <div>
      <div className="text-xs uppercase tracking-wide text-muted">{label}</div>
      {keys && keys.length > 0 ? (
        <ul className="mt-1 space-y-1">
          {keys.map((key) => (
            <li key={`${key.keyType}:${key.fingerprint}`} className="break-all font-mono text-xs">
              <span className="text-muted">{key.keyType} </span>
              <span className={tone === "danger" ? "text-danger" : "text-foreground"}>
                {key.fingerprint}
              </span>
            </li>
          ))}
        </ul>
      ) : (
        <div className="mt-1 text-xs text-muted">Not available.</div>
      )}
    </div>
  );
}
