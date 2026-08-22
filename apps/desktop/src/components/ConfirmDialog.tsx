import { useEffect, useState } from "react";
import { Modal } from "./Modal";
import { cn } from "../lib/utils";

/*
 * A confirmation modal. Reuses Modal so it inherits focus trapping and Escape.
 */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  message,
  confirmLabel = "Confirm",
  cancelLabel = "Cancel",
  destructive = false,
  busy = false,
  requireTyped,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  message: React.ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  destructive?: boolean;
  busy?: boolean;
  /** When set, the confirm button unlocks only once this exact word is typed.
   * For actions that are irreversible or affect other people. */
  requireTyped?: string;
  onConfirm: () => void;
}) {
  const [typed, setTyped] = useState("");

  // Reopening must not inherit the previous attempt's typed confirmation.
  useEffect(() => {
    if (!open) setTyped("");
  }, [open]);

  const locked = requireTyped !== undefined && typed !== requireTyped;
  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title={title}
      size="sm"
      footer={
        <>
          <button
            type="button"
            onClick={() => onOpenChange(false)}
            className="rounded-md border border-border px-3 py-1.5 text-sm text-muted hover:text-foreground"
          >
            {cancelLabel}
          </button>
          <button
            type="button"
            onClick={onConfirm}
            disabled={busy || locked}
            className={cn(
              "rounded-md px-3 py-1.5 text-sm font-medium disabled:opacity-50",
              destructive
                ? "bg-danger text-white hover:brightness-110"
                : "bg-accent text-accent-foreground",
            )}
          >
            {confirmLabel}
          </button>
        </>
      }
    >
      <div className="text-sm text-muted">{message}</div>
      {requireTyped !== undefined && (
        <label className="mt-3 block">
          <span className="text-xs text-muted">
            Type <span className="font-medium text-foreground">{requireTyped}</span> to
            confirm.
          </span>
          <input
            value={typed}
            onChange={(event) => setTyped(event.target.value)}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="none"
            spellCheck={false}
            className="mt-1.5 w-full rounded-md border border-border bg-background px-2.5 py-1.5 font-mono text-sm text-foreground outline-none focus:border-accent"
          />
        </label>
      )}
    </Modal>
  );
}
