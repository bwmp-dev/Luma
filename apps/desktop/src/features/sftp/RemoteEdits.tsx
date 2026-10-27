import { useEffect, useMemo, useState } from "react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronUp,
  CircleAlert,
  CloudOff,
  ExternalLink,
  FilePen,
  FolderOpen,
  History,
  LoaderCircle,
  MoreHorizontal,
  RotateCcw,
  Upload,
  X,
  type LucideIcon,
} from "lucide-react";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import {
  ENCODING_LABELS,
  remoteEditClose,
  remoteEditReveal,
  remoteEditUpload,
  type RemoteEdit,
  type RemoteEditStatus,
} from "../../lib/remoteEdit";
import { cn } from "../../lib/utils";
import { editErrorMessage, useRemoteEditStore } from "../../stores/remoteEditStore";
import { EditorPicker } from "./EditorPicker";
import { RemoteEditConflict } from "./RemoteEditConflict";
import { MENU_CONTENT_CLASS, MENU_ITEM_CLASS } from "./ViewMenu";

/*
 * Files open in an external editor, docked above the transfer queue. Each row
 * mirrors one backend edit: the backend watches the working copy and uploads
 * every save, and this panel only shows where that stands and offers the
 * decisions the backend will not make alone — a conflict, a recovered edit,
 * a failed upload, or stopping with changes that never reached the host.
 */

/** The label stays in the foreground colour so it reads on the tint in both
 * themes; the icon carries the status colour. */
const STATUS_CHIP: Record<
  RemoteEditStatus,
  { label: string; icon: LucideIcon; chip: string; tone: string }
> = {
  synced: { label: "Synced", icon: Check, chip: "bg-success/15", tone: "text-success" },
  uploading: { label: "Uploading", icon: LoaderCircle, chip: "bg-accent/15", tone: "text-accent" },
  offline: {
    label: "Waiting for connection",
    icon: CloudOff,
    chip: "bg-muted/15",
    tone: "text-muted",
  },
  conflict: { label: "Conflict", icon: AlertTriangle, chip: "bg-warning/15", tone: "text-warning" },
  error: { label: "Not uploaded", icon: CircleAlert, chip: "bg-danger/15", tone: "text-danger" },
  recovered: { label: "Recovered", icon: History, chip: "bg-warning/15", tone: "text-warning" },
};

const NEEDS_ATTENTION = new Set<RemoteEditStatus>(["conflict", "error", "recovered"]);

const ROW_BUTTON =
  "flex shrink-0 items-center gap-1 whitespace-nowrap rounded-md border px-2 py-1 text-[11px] font-medium disabled:opacity-50";

function detailFor(edit: RemoteEdit): string {
  if (edit.message) return edit.message;
  if (edit.status === "uploading") return "Uploading your latest save…";
  const parts = [ENCODING_LABELS[edit.encoding]];
  if (edit.lastUploadedAt) {
    parts.push(
      `uploaded ${new Date(edit.lastUploadedAt).toLocaleTimeString(undefined, {
        hour: "2-digit",
        minute: "2-digit",
      })}`,
    );
  } else {
    parts.push("save in your editor to upload");
  }
  if (!edit.connected) parts.push("not connected");
  return parts.join(" · ");
}

export function RemoteEdits({ hostName }: { hostName: (hostId: string) => string }) {
  const edits = useRemoteEditStore((s) => s.edits);
  const opening = useRemoteEditStore((s) => s.opening);
  const openError = useRemoteEditStore((s) => s.openError);
  const loadError = useRemoteEditStore((s) => s.loadError);
  const picker = useRemoteEditStore((s) => s.picker);
  const init = useRemoteEditStore((s) => s.init);
  const reopen = useRemoteEditStore((s) => s.reopen);
  const pick = useRemoteEditStore((s) => s.pick);
  const cancelPicker = useRemoteEditStore((s) => s.cancelPicker);
  const clearOpenError = useRemoteEditStore((s) => s.clearOpenError);

  const [collapsed, setCollapsed] = useState(false);
  const [reviewingId, setReviewingId] = useState<string | null>(null);
  const [closing, setClosing] = useState<RemoteEdit | null>(null);
  const [rowErrors, setRowErrors] = useState<Record<string, string>>({});

  useEffect(() => {
    void init();
  }, [init]);

  const rows = useMemo(
    () =>
      Object.values(edits).sort(
        (left, right) =>
          left.fileName.localeCompare(right.fileName) || left.editId.localeCompare(right.editId),
      ),
    [edits],
  );
  const pending = Object.entries(opening);
  const attention = rows.filter((edit) => NEEDS_ATTENTION.has(edit.status)).length;
  const reviewing = reviewingId ? edits[reviewingId] : undefined;

  /** Run a row action, keeping its failure on that row. */
  const run = async (edit: RemoteEdit, action: () => Promise<void>) => {
    setRowErrors((current) => {
      const next = { ...current };
      delete next[edit.editId];
      return next;
    });
    try {
      await action();
    } catch (error) {
      setRowErrors((current) => ({ ...current, [edit.editId]: editErrorMessage(error) }));
    }
  };

  const stopEditing = (edit: RemoteEdit) => {
    if (edit.unsynced) {
      setClosing(edit);
      return;
    }
    void run(edit, () => remoteEditClose(edit.editId, false));
  };

  const pickerDialog = picker && (
    <EditorPicker
      key={picker.target.kind === "open" ? picker.target.request.path : picker.target.editId}
      fileName={
        picker.target.kind === "open" ? picker.target.request.fileName : picker.target.fileName
      }
      editors={picker.editors}
      preferredId={picker.preferredId}
      offerRemember={picker.target.kind === "open"}
      onCancel={cancelPicker}
      onPick={(editorId, remember) => void pick(editorId, remember)}
    />
  );

  if (rows.length === 0 && pending.length === 0 && !openError && !loadError) return pickerDialog;

  return (
    <div className="shrink-0 border-t border-border bg-surface">
      <div className="flex items-center gap-2 px-3 py-2">
        <button
          type="button"
          onClick={() => setCollapsed((value) => !value)}
          aria-expanded={!collapsed}
          className="flex items-center gap-2 text-xs font-semibold text-foreground"
        >
          {collapsed ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
          Editing
          {rows.length > 0 && (
            <span className="font-normal text-muted">
              {rows.length} file{rows.length === 1 ? "" : "s"}
            </span>
          )}
          {attention > 0 && (
            <span className="rounded-full bg-warning/15 px-2 py-0.5 text-[10px] font-medium text-warning">
              {attention} need{attention === 1 ? "s" : ""} attention
            </span>
          )}
        </button>
      </div>

      {!collapsed && (
        <ul className="max-h-48 overflow-y-auto px-2 pb-2">
          {loadError && (
            <li
              role="alert"
              className="flex items-start gap-3 rounded-md px-2 py-1.5 text-xs text-danger"
            >
              <span className="min-w-0 flex-1">
                Couldn't load the files open for editing: {loadError}
              </span>
              <button
                type="button"
                onClick={() => void init()}
                className={cn(ROW_BUTTON, "border-border text-foreground hover:border-accent")}
              >
                <RotateCcw size={12} /> Retry
              </button>
            </li>
          )}
          {openError && (
            <li
              role="alert"
              className="flex items-start gap-3 rounded-md px-2 py-1.5 text-xs text-danger"
            >
              <span className="min-w-0 flex-1">
                Couldn't open <span className="font-medium">{openError.fileName}</span>:{" "}
                {openError.message}
              </span>
              <button
                type="button"
                onClick={clearOpenError}
                aria-label="Dismiss"
                className="shrink-0 rounded p-0.5 text-muted hover:text-foreground"
              >
                <X size={13} />
              </button>
            </li>
          )}
          {pending.map(([key, fileName]) => (
            <li key={key} className="flex items-center gap-3 px-2 py-1.5 text-xs text-muted">
              <LoaderCircle size={14} className="shrink-0 motion-safe:animate-spin" />
              Opening {fileName}…
            </li>
          ))}
          {rows.map((edit) => (
            <EditRow
              key={edit.editId}
              edit={edit}
              hostName={hostName(edit.hostId)}
              error={rowErrors[edit.editId] ?? null}
              onReview={() => setReviewingId(edit.editId)}
              onUpload={() => void run(edit, () => remoteEditUpload(edit.editId))}
              onReopen={(choose) => void reopen(edit, choose)}
              onReveal={(backup) => void run(edit, () => remoteEditReveal(edit.editId, backup))}
              onStop={() => stopEditing(edit)}
            />
          ))}
        </ul>
      )}

      {reviewing?.status === "conflict" && (
        <RemoteEditConflict
          key={reviewing.editId}
          edit={reviewing}
          hostName={hostName(reviewing.hostId)}
          onClose={() => setReviewingId(null)}
        />
      )}

      <ConfirmDialog
        open={closing !== null}
        onOpenChange={(open) => !open && setClosing(null)}
        title="Discard changes that weren't uploaded?"
        destructive
        confirmLabel="Discard changes"
        cancelLabel="Keep editing"
        onConfirm={() => {
          if (closing) void run(closing, () => remoteEditClose(closing.editId, true));
          setClosing(null);
        }}
        message={
          <>
            <span className="font-medium text-foreground">{closing?.fileName}</span> has saved
            changes that aren't on {closing ? hostName(closing.hostId) : "the host"} yet. Stopping
            now deletes the working copy and those changes.
          </>
        }
      />

      {pickerDialog}
    </div>
  );
}

function EditRow({
  edit,
  hostName,
  error,
  onReview,
  onUpload,
  onReopen,
  onReveal,
  onStop,
}: {
  edit: RemoteEdit;
  hostName: string;
  error: string | null;
  onReview: () => void;
  onUpload: () => void;
  onReopen: (choose: boolean) => void;
  onReveal: (backup: boolean) => void;
  onStop: () => void;
}) {
  const chip = STATUS_CHIP[edit.status];
  const ChipIcon = chip.icon;
  const detail = detailFor(edit);
  const detailTone =
    edit.status === "error"
      ? "text-danger"
      : edit.status === "conflict" || edit.status === "recovered"
        ? "text-warning"
        : "text-muted";

  return (
    <li className="rounded-md px-2 py-1.5 text-xs hover:bg-raised">
      <div className="flex items-center gap-3">
        <FilePen size={14} className="shrink-0 text-muted" />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate font-medium text-foreground" title={edit.fileName}>
              {edit.fileName}
            </span>
            <span
              className={cn(
                "flex shrink-0 items-center gap-1 whitespace-nowrap rounded-full px-1.5 py-0.5 text-[10px] font-medium text-foreground",
                chip.chip,
              )}
            >
              <ChipIcon
                size={10}
                aria-hidden
                className={cn(
                  "shrink-0",
                  chip.tone,
                  edit.status === "uploading" && "motion-safe:animate-spin",
                )}
              />
              {chip.label}
            </span>
          </div>
          <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-[10px] text-muted">
            <span className="shrink-0">{hostName}</span>
            <span aria-hidden>·</span>
            <span className="truncate font-mono" title={edit.remotePath}>
              {edit.remotePath}
            </span>
          </div>
          <p className={cn("mt-0.5 text-[10px]", detailTone)}>{detail}</p>
          {error && (
            <p role="alert" className="mt-0.5 text-[10px] text-danger">
              {error}
            </p>
          )}
        </div>

        {edit.status === "conflict" && (
          <button
            type="button"
            onClick={onReview}
            className={cn(ROW_BUTTON, "border-warning/50 text-warning hover:bg-warning/10")}
          >
            Review changes
          </button>
        )}
        {edit.status === "recovered" && (
          <button
            type="button"
            onClick={onUpload}
            className={cn(ROW_BUTTON, "border-border text-foreground hover:border-accent")}
          >
            <Upload size={12} /> Upload
          </button>
        )}
        {edit.status === "error" && edit.unsynced && (
          <button
            type="button"
            onClick={onUpload}
            className={cn(ROW_BUTTON, "border-border text-foreground hover:border-accent")}
          >
            <RotateCcw size={12} /> Retry
          </button>
        )}

        <button
          type="button"
          onClick={() => onReopen(false)}
          aria-label={`Open ${edit.fileName} in editor`}
          title="Open in editor"
          className="shrink-0 rounded p-1 text-muted hover:text-accent"
        >
          <ExternalLink size={14} />
        </button>
        <DropdownMenu.Root>
          <DropdownMenu.Trigger asChild>
            <button
              type="button"
              aria-label={`${edit.fileName} editing actions`}
              className="shrink-0 rounded p-1 text-muted hover:text-foreground"
            >
              <MoreHorizontal size={14} />
            </button>
          </DropdownMenu.Trigger>
          <DropdownMenu.Portal>
            <DropdownMenu.Content
              align="end"
              sideOffset={4}
              className={cn(MENU_CONTENT_CLASS, "min-w-44")}
            >
              <DropdownMenu.Item
                onSelect={() => onReopen(true)}
                className={cn(MENU_ITEM_CLASS, "py-1.5")}
              >
                <ExternalLink size={14} /> Open with…
              </DropdownMenu.Item>
              <DropdownMenu.Item
                onSelect={() => onReveal(false)}
                className={cn(MENU_ITEM_CLASS, "py-1.5")}
              >
                <FolderOpen size={14} /> Show working copy
              </DropdownMenu.Item>
              {edit.backupPath && (
                <DropdownMenu.Item
                  onSelect={() => onReveal(true)}
                  className={cn(MENU_ITEM_CLASS, "py-1.5")}
                >
                  <History size={14} /> Show backup of my version
                </DropdownMenu.Item>
              )}
              <DropdownMenu.Separator className="my-1 h-px bg-border" />
              <DropdownMenu.Item
                onSelect={onStop}
                className={cn(MENU_ITEM_CLASS, "py-1.5", edit.unsynced && "text-danger")}
              >
                <X size={14} /> {edit.unsynced ? "Discard and stop editing" : "Stop editing"}
              </DropdownMenu.Item>
            </DropdownMenu.Content>
          </DropdownMenu.Portal>
        </DropdownMenu.Root>
      </div>
    </li>
  );
}
