import { useEffect, useState } from "react";
import { Check, ClipboardCopy, ExternalLink, TextSelect, X } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { terminalManager } from "../terminal/terminalManager";
import { isUrlText } from "../terminal/bufferText";
import { cn } from "../../lib/utils";

/*
 * The bar that accompanies mobile selection mode (see useTerminalSelection.ts).
 * It reports what the current drag or tap picked up and offers the two things
 * there is otherwise no touch affordance for: copying it, and opening it when it
 * is a link. Reads the selection straight off the terminal — no bytes through
 * React state, only the already-selected text.
 */

export function MobileSelectionBar({
  sessionId,
  onDone,
}: {
  sessionId: string;
  onDone: () => void;
}) {
  const [selection, setSelection] = useState("");
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    const read = () => setSelection(terminalManager.readSelection(sessionId));
    read();
    return terminalManager.onSelectionChange(sessionId, read);
  }, [sessionId]);

  useEffect(() => {
    setCopied(false);
  }, [selection]);

  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1400);
    return () => window.clearTimeout(timer);
  }, [copied]);

  const trimmed = selection.trim();
  const url = isUrlText(trimmed) ? trimmed : null;
  const empty = selection.length === 0;

  const copy = () => {
    // No refocus: raising the soft keyboard would hide the output the user is
    // still selecting from.
    terminalManager.copySelection(sessionId, false);
    setCopied(true);
  };

  return (
    <div className="shrink-0 border-t border-border bg-surface px-2 py-1.5">
      <div className="flex items-center gap-2">
        <p className="min-w-0 flex-1 truncate text-[11px] text-muted">
          {empty
            ? "Drag to select · tap a link or word"
            : copied
              ? "Copied"
              : url
                ? url
                : `${selection.length} character${selection.length === 1 ? "" : "s"} selected`}
        </p>
        {url && (
          <BarButton label="Open link" onPress={() => void openUrl(url)}>
            <ExternalLink size={15} />
          </BarButton>
        )}
        <BarButton label="Copy" disabled={empty} onPress={copy}>
          {copied ? <Check size={15} /> : <ClipboardCopy size={15} />}
        </BarButton>
        <BarButton
          label="Select all"
          onPress={() => terminalManager.selectAll(sessionId, false)}
        >
          <TextSelect size={15} />
        </BarButton>
        <BarButton label="Done selecting" onPress={onDone}>
          <X size={15} />
        </BarButton>
      </div>
    </div>
  );
}

function BarButton({
  children,
  label,
  disabled,
  onPress,
}: {
  children: React.ReactNode;
  label: string;
  disabled?: boolean;
  onPress: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onPress}
      className={cn(
        "flex h-9 w-9 shrink-0 items-center justify-center rounded-md border border-border bg-raised text-muted active:bg-accent/20",
        disabled && "opacity-40",
      )}
    >
      {children}
    </button>
  );
}
